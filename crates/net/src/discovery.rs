use std::net::SocketAddr;
use std::time::Duration;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
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
}

impl DiscoveryBeacon {
    pub fn new(device_name: String, role: DeviceRole, input_port: u16, video_port: u16) -> Self {
        Self {
            magic: DISCOVERY_MAGIC,
            version: 1,
            device_name,
            role,
            input_port,
            video_port,
        }
    }
}

pub struct DiscoveryBeaconEmitter {
    socket: UdpSocket,
    broadcast_addr: SocketAddr,
    beacon: DiscoveryBeacon,
}

impl DiscoveryBeaconEmitter {
    pub async fn new(beacon: DiscoveryBeacon) -> Result<Self> {
        let std_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| NerveError::Io(e))?;

        let _ = std_socket.set_broadcast(true);
        let _ = std_socket.set_nonblocking(true);

        let bind_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        std_socket
            .bind(&bind_addr.into())
            .map_err(|e| NerveError::Io(e))?;

        let socket = UdpSocket::from_std(std_socket.into())
            .map_err(|e| NerveError::Io(e))?;

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
        let data = serde_json::to_vec(&self.beacon)
            .map_err(|e| NerveError::Serialization(e.to_string()))?;

        self.socket
            .send_to(&data, self.broadcast_addr)
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

        let _ = std_socket.set_broadcast(true);
        let _ = std_socket.set_reuse_address(true);
        let _ = std_socket.set_nonblocking(true);

        let bind_addr: SocketAddr = format!("0.0.0.0:{}", DISCOVERY_PORT)
            .parse()
            .unwrap();

        std_socket
            .bind(&bind_addr.into())
            .map_err(|e| NerveError::Io(e))?;

        let socket = UdpSocket::from_std(std_socket.into())
            .map_err(|e| NerveError::Io(e))?;

        info!("Discovery listener listening on 0.0.0.0:{}", DISCOVERY_PORT);

        Ok(Self { socket })
    }

    pub async fn recv_beacon(&self) -> Result<(DiscoveryBeacon, SocketAddr)> {
        let mut buf = [0u8; 1024];

        loop {
            let (len, peer) = self
                .socket
                .recv_from(&mut buf)
                .await
                .map_err(|e| NerveError::Io(e))?;

            if let Ok(beacon) = serde_json::from_slice::<DiscoveryBeacon>(&buf[..len]) {
                if beacon.magic == DISCOVERY_MAGIC {
                    return Ok((beacon, peer));
                }
            }
        }
    }
}
