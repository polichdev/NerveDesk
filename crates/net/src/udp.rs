use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tracing::{trace, warn};

use crate::secure_input::{seal, ReplayGuard, MAX_SECURE_INPUT_BYTES};
use common::{
    decode_packet, encode_packet, InputEvent, NerveError, PacketHeader, Result, MAX_PACKET_SIZE,
};

pub struct UdpInputSender {
    socket: UdpSocket,
    target_addr: SocketAddr,
    sequence: AtomicU64,
    security_key: String,
}

impl UdpInputSender {
    pub async fn new(target_addr: SocketAddr, security_key: String) -> Result<Self> {
        if security_key.len() < 16 || security_key.len() > 256 {
            return Err(NerveError::Protocol(
                "A 16-256 character pairing key is required".into(),
            ));
        }
        let std_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| NerveError::Io(e))?;

        let _ = std_socket.set_send_buffer_size(64 * 1024);
        let _ = std_socket.set_nonblocking(true);

        let any_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        std_socket
            .bind(&any_addr.into())
            .map_err(|e| NerveError::Io(e))?;

        let socket = UdpSocket::from_std(std_socket.into()).map_err(|e| NerveError::Io(e))?;

        Ok(Self {
            socket,
            target_addr,
            sequence: AtomicU64::new(1),
            security_key,
        })
    }

    #[inline]
    fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }

    #[inline]
    fn current_timestamp_us() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }

    pub async fn send_event(&self, event: &InputEvent) -> Result<usize> {
        let mut buf = [0u8; MAX_PACKET_SIZE];
        let seq = self.next_sequence();
        let ts = Self::current_timestamp_us();

        let len = encode_packet(event, seq, ts, &mut buf)?;
        let packet = seal(&self.security_key, &buf[..len]).map_err(NerveError::Protocol)?;
        let sent = self
            .socket
            .send_to(&packet, self.target_addr)
            .await
            .map_err(|e| NerveError::Io(e))?;

        trace!(
            "Sent UDP input packet seq={} bytes={} to {}",
            seq,
            sent,
            self.target_addr
        );
        Ok(sent)
    }

    pub fn try_send_event(&self, event: &InputEvent) -> Result<usize> {
        let mut buf = [0u8; MAX_PACKET_SIZE];
        let seq = self.next_sequence();
        let ts = Self::current_timestamp_us();

        let len = encode_packet(event, seq, ts, &mut buf)?;
        let packet = seal(&self.security_key, &buf[..len]).map_err(NerveError::Protocol)?;
        let sent = self
            .socket
            .try_send_to(&packet, self.target_addr)
            .map_err(|e| NerveError::Io(e))?;

        Ok(sent)
    }

    pub fn target_addr(&self) -> SocketAddr {
        self.target_addr
    }
}

pub struct UdpInputReceiver {
    socket: UdpSocket,
    security_key: Arc<Mutex<Option<String>>>,
    paired_peer_ip: Arc<Mutex<Option<String>>>,
    replay_guard: ReplayGuard,
    probe_replay: ReplayGuard,
}

impl UdpInputReceiver {
    pub async fn bind(
        bind_addr: SocketAddr,
        security_key: Arc<Mutex<Option<String>>>,
        paired_peer_ip: Arc<Mutex<Option<String>>>,
    ) -> Result<Self> {
        let std_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| NerveError::Io(e))?;

        let _ = std_socket.set_recv_buffer_size(64 * 1024);
        let _ = std_socket.set_nonblocking(true);

        std_socket
            .bind(&bind_addr.into())
            .map_err(|e| NerveError::Io(e))?;

        let socket = UdpSocket::from_std(std_socket.into()).map_err(|e| NerveError::Io(e))?;

        Ok(Self {
            socket,
            security_key,
            paired_peer_ip,
            replay_guard: ReplayGuard::new(),
            probe_replay: ReplayGuard::new(),
        })
    }

    pub async fn recv_event(&mut self) -> Result<(PacketHeader, InputEvent, SocketAddr)> {
        let mut buf = [0u8; MAX_SECURE_INPUT_BYTES];

        loop {
            let (len, peer) = self
                .socket
                .recv_from(&mut buf)
                .await
                .map_err(|e| NerveError::Io(e))?;

            let paired = self.paired_peer_ip.lock().ok().and_then(|ip| ip.clone());
            let peer_ip = peer.ip().to_string();
            if paired.as_deref().is_some_and(|ip| ip != peer_ip) {
                continue;
            }
            let key = self.security_key.lock().ok().and_then(|key| key.clone());
            let Some(key) = key else { continue };
            if self
                .probe_replay
                .open_payload(&key, b"pair-probe", &buf[..len], 32)
                .ok()
                .as_deref()
                == Some(b"NerveDesk".as_slice())
            {
                if paired.is_none() {
                    if let Ok(mut configured) = self.paired_peer_ip.lock() {
                        if configured.is_none() {
                            *configured = Some(peer_ip);
                        }
                    }
                }
                if let Ok(ack) =
                    crate::secure_input::seal_payload(&key, b"pair-ack", &buf[..12], 32)
                {
                    let _ = self.socket.send_to(&ack, peer).await;
                }
                continue;
            }
            if paired.is_none() {
                continue;
            }
            let plaintext = match self.replay_guard.open(&key, &buf[..len]) {
                Ok(plaintext) => plaintext,
                Err(_) => continue,
            };
            match decode_packet(&plaintext) {
                Ok((header, event)) => {
                    return Ok((header, event, peer));
                }
                Err(err) => {
                    warn!("Failed to decode UDP packet from {}: {:?}", peer, err);
                    continue;
                }
            }
        }
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}
