use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;
use tracing::{info, trace, warn};

use common::{NerveError, Result};

type HmacSha256 = Hmac<Sha256>;

fn compute_hmac(payload: &[u8], key: &str) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(payload);
    mac.finalize().into_bytes().into()
}

fn encode_beacon(beacon: &DiscoveryBeacon, pairing_key: Option<&str>) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(beacon)
        .map_err(|e| NerveError::Serialization(e.to_string()))?;
    match pairing_key {
        None => Ok(payload),
        Some(key) => {
            let sig = compute_hmac(&payload, key);
            let mut packet = Vec::with_capacity(4 + payload.len() + 32);
            packet.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            packet.extend_from_slice(&payload);
            packet.extend_from_slice(&sig);
            Ok(packet)
        }
    }
}

fn decode_beacon(data: &[u8], pairing_key: Option<&str>) -> Result<DiscoveryBeacon> {

    if data.len() > 4 + 32 {
        let payload_len = u32::from_le_bytes(data[..4].try_into().unwrap()) as usize;
        if payload_len + 4 + 32 == data.len() {
            let payload = &data[4..4 + payload_len];
            let received_sig = &data[4 + payload_len..];
            if let Some(key) = pairing_key {

                let expected = compute_hmac(payload, key);
                if !constant_time_eq(received_sig, &expected) {
                    return Err(NerveError::Protocol(
                        "Discovery beacon HMAC verification failed".into(),
                    ));
                }
            }

            let beacon: DiscoveryBeacon = serde_json::from_slice(payload)
                .map_err(|e| NerveError::Serialization(e.to_string()))?;
            if beacon.magic != DISCOVERY_MAGIC {
                return Err(NerveError::Protocol(
                    "Invalid discovery beacon magic".into(),
                ));
            }
            return Ok(beacon);
        }
    }

    if pairing_key.is_some() {

        return Err(NerveError::Protocol(
            "Unsigned discovery beacon rejected in paired mode".into(),
        ));
    }
    let beacon: DiscoveryBeacon = serde_json::from_slice(data)
        .map_err(|e| NerveError::Serialization(e.to_string()))?;
    if beacon.magic != DISCOVERY_MAGIC {
        return Err(NerveError::Protocol(
            "Invalid discovery beacon magic".into(),
        ));
    }
    Ok(beacon)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub const DISCOVERY_PORT: u16 = 48103;
pub const DISCOVERY_MAGIC: [u8; 4] = *b"NRVD";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceRole {
    Host,
    Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryBeacon {
    pub magic: [u8; 4],
    pub version: u8,
    pub device_name: String,
    pub role: DeviceRole,
    pub input_port: u16,
    pub video_port: u16,
    pub screen_width: u32,
    pub screen_height: u32,
    #[serde(default = "default_screen_edge")]
    pub screen_edge: String,
}

impl DiscoveryBeacon {
    pub fn new(
        device_name: String,
        role: DeviceRole,
        input_port: u16,
        video_port: u16,
        screen_width: u32,
        screen_height: u32,
    ) -> Self {
        Self {
            magic: DISCOVERY_MAGIC,
            version: 1,
            device_name,
            role,
            input_port,
            video_port,
            screen_width,
            screen_height,
            screen_edge: default_screen_edge(),
        }
    }

    pub fn with_edge(mut self, edge: String) -> Self {
        self.screen_edge = edge;
        self
    }
}

fn default_screen_edge() -> String {
    "Right".into()
}

pub struct DiscoveryBeaconEmitter {
    socket: UdpSocket,
    broadcast_addr: SocketAddr,
    beacon: DiscoveryBeacon,

    pairing_key: Option<String>,
}

impl DiscoveryBeaconEmitter {
    pub fn set_screen_edge(&mut self, edge: String) {
        self.beacon.screen_edge = edge;
    }

    pub fn set_pairing_key(&mut self, key: Option<String>) {
        self.pairing_key = key;
    }

    pub async fn new(beacon: DiscoveryBeacon) -> Result<Self> {
        let std_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| NerveError::Io(e))?;

        let _ = std_socket.set_broadcast(true);
        let _ = std_socket.set_nonblocking(true);

        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        std_socket
            .bind(&bind_addr.into())
            .map_err(|e| NerveError::Io(e))?;

        let socket = UdpSocket::from_std(std_socket.into()).map_err(|e| NerveError::Io(e))?;

        let broadcast_addr: SocketAddr = format!("255.255.255.255:{}", DISCOVERY_PORT)
            .parse()
            .unwrap();

        Ok(Self {
            socket,
            broadcast_addr,
            beacon,
            pairing_key: None,
        })
    }

    pub async fn emit_beacon(&self) -> Result<()> {
        let data = encode_beacon(&self.beacon, None)?;
        self.socket
            .send_to(&data, self.broadcast_addr)
            .await
            .map_err(NerveError::Io)?;
        trace!("Emitted unsigned broadcast beacon for {}", self.beacon.device_name);
        Ok(())
    }

    pub async fn emit_to(&self, address: SocketAddr) -> Result<()> {
        let key = self.pairing_key.as_deref();
        let data = encode_beacon(&self.beacon, key)?;
        self.socket
            .send_to(&data, address)
            .await
            .map_err(NerveError::Io)?;
        trace!(
            "Emitted {} beacon for {} to {}",
            if key.is_some() { "signed" } else { "unsigned" },
            self.beacon.device_name,
            address,
        );
        Ok(())
    }

    pub fn spawn_loop(self, interval: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                if let Err(e) = self.emit_beacon().await {
                    warn!("Failed to send discovery beacon: {:?}", e);
                }
            }
        })
    }
}

pub struct DiscoveryListener {
    socket: UdpSocket,
}

impl DiscoveryListener {
    pub async fn bind() -> Result<Self> {
        let std_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| NerveError::Io(e))?;

        let _ = std_socket.set_reuse_address(true);
        let _ = std_socket.set_nonblocking(true);

        let bind_addr: SocketAddr = format!("0.0.0.0:{}", DISCOVERY_PORT).parse().unwrap();
        std_socket
            .bind(&bind_addr.into())
            .map_err(|e| NerveError::Io(e))?;

        let socket = UdpSocket::from_std(std_socket.into()).map_err(|e| NerveError::Io(e))?;

        info!("Discovery listener bound to {}", bind_addr);
        Ok(Self { socket })
    }

    pub async fn recv_beacon(
        &self,
        pairing_key: Option<&str>,
    ) -> Result<(DiscoveryBeacon, SocketAddr)> {
        let mut buf = [0u8; 1024];
        let (len, peer_addr) = self
            .socket
            .recv_from(&mut buf)
            .await
            .map_err(|e| NerveError::Io(e))?;

        let beacon = decode_beacon(&buf[..len], pairing_key)?;

        trace!("Received beacon from {}: {:?}", peer_addr, beacon);
        Ok((beacon, peer_addr))
    }

    pub async fn reply_to(
        &self,
        beacon: &DiscoveryBeacon,
        address: SocketAddr,
        pairing_key: Option<&str>,
    ) -> Result<()> {
        let data = encode_beacon(beacon, pairing_key)
            .map_err(|e| NerveError::Serialization(e.to_string()))?;
        self.socket
            .send_to(&data, address)
            .await
            .map_err(NerveError::Io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_edge_and_accepts_older_beacons() {
        let beacon =
            DiscoveryBeacon::new("Laptop".into(), DeviceRole::Host, 48100, 48101, 1600, 900)
                .with_edge("Top".into());
        let encoded = serde_json::to_value(&beacon).unwrap();
        assert_eq!(
            serde_json::from_value::<DiscoveryBeacon>(encoded.clone())
                .unwrap()
                .screen_edge,
            "Top"
        );
        let mut older = encoded;
        older.as_object_mut().unwrap().remove("screen_edge");
        assert_eq!(
            serde_json::from_value::<DiscoveryBeacon>(older)
                .unwrap()
                .screen_edge,
            "Right"
        );
    }

    #[test]
    fn signed_beacon_roundtrip_and_tamper_rejection() {
        let beacon =
            DiscoveryBeacon::new("TestPC".into(), DeviceRole::Host, 48100, 48101, 1920, 1080);
        let key = "super-secret-pairing-key";

        let packet = encode_beacon(&beacon, Some(key)).unwrap();
        let decoded = decode_beacon(&packet, Some(key)).unwrap();
        assert_eq!(decoded.device_name, "TestPC");

        assert!(decode_beacon(&packet, Some("wrong-key")).is_err());

        let mut tampered = packet.clone();
        tampered[5] ^= 0xFF;
        assert!(decode_beacon(&tampered, Some(key)).is_err());

        let plain = serde_json::to_vec(&beacon).unwrap();
        assert!(decode_beacon(&plain, Some(key)).is_err());

        decode_beacon(&plain, None).unwrap();
    }

    #[test]
    fn constant_time_eq_is_correct() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hi", b"hello"));
    }
}
