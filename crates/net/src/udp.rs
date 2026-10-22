use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tracing::{trace, warn};

use common::{
    decode_packet, encode_packet, InputEvent, NerveError, PacketHeader, Result, MAX_PACKET_SIZE,
};

pub struct UdpInputSender {
    socket: UdpSocket,
    target_addr: SocketAddr,
    sequence: AtomicU64,
}

impl UdpInputSender {
    pub async fn new(target_addr: SocketAddr) -> Result<Self> {
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
        let sent = self
            .socket
            .send_to(&buf[..len], self.target_addr)
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
        let sent = self
            .socket
            .try_send_to(&buf[..len], self.target_addr)
            .map_err(|e| NerveError::Io(e))?;

        Ok(sent)
    }

    pub fn target_addr(&self) -> SocketAddr {
        self.target_addr
    }
}

pub struct UdpInputReceiver {
    socket: UdpSocket,
    last_sequence: u64,
}

impl UdpInputReceiver {
    pub async fn bind(bind_addr: SocketAddr) -> Result<Self> {
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
            last_sequence: 0,
        })
    }

    pub async fn recv_event(&mut self) -> Result<(PacketHeader, InputEvent, SocketAddr)> {
        let mut buf = [0u8; MAX_PACKET_SIZE];

        loop {
            let (len, peer) = self
                .socket
                .recv_from(&mut buf)
                .await
                .map_err(|e| NerveError::Io(e))?;

            match decode_packet(&buf[..len]) {
                Ok((header, event)) => {
                    if header.sequence <= self.last_sequence
                        && self.last_sequence.saturating_sub(header.sequence) < 5000
                        && header.sequence > 10
                    {
                        continue;
                    }

                    self.last_sequence = header.sequence;
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
