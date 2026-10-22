use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::UdpSocket;
use tracing::{info, trace, warn};

use common::{NerveError, Result};

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
}

impl DiscoveryBeaconEmitter {
    pub fn set_screen_edge(&mut self, edge: String) {
        self.beacon.screen_edge = edge;
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
        })
    }

    pub async fn emit_beacon(&self) -> Result<()> {
        self.emit_to(self.broadcast_addr).await
    }

    pub async fn emit_to(&self, address: SocketAddr) -> Result<()> {
        let data = serde_json::to_vec(&self.beacon)
            .map_err(|e| NerveError::Serialization(e.to_string()))?;

        self.socket
            .send_to(&data, address)
            .await
            .map_err(|e| NerveError::Io(e))?;

        trace!("Emitted discovery beacon for {}", self.beacon.device_name);
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

    pub async fn recv_beacon(&self) -> Result<(DiscoveryBeacon, SocketAddr)> {
        let mut buf = [0u8; 1024];
        let (len, peer_addr) = self
            .socket
            .recv_from(&mut buf)
            .await
            .map_err(|e| NerveError::Io(e))?;

        let beacon: DiscoveryBeacon = serde_json::from_slice(&buf[..len])
            .map_err(|e| NerveError::Serialization(e.to_string()))?;

        if beacon.magic != DISCOVERY_MAGIC {
            return Err(NerveError::Protocol(
                "Invalid discovery beacon magic".into(),
            ));
        }

        trace!("Received beacon from {}: {:?}", peer_addr, beacon);
        Ok((beacon, peer_addr))
    }

    pub async fn reply_to(&self, beacon: &DiscoveryBeacon, address: SocketAddr) -> Result<()> {
        let data =
            serde_json::to_vec(beacon).map_err(|e| NerveError::Serialization(e.to_string()))?;
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
}
