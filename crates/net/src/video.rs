use socket2::{Domain, Protocol, Socket, Type};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::UdpSocket;
use tracing::trace;

use codec::{EncodedVideoFrame, VideoFrameChunker, VideoFrameReassembler};
use common::{NerveError, Result, VideoChunkHeader, MAX_VIDEO_PAYLOAD_SIZE};

pub struct VideoSender {
    socket: UdpSocket,
    target_addr: SocketAddr,
    security_key: String,
}

impl VideoSender {
    pub async fn new(target_addr: SocketAddr, security_key: String) -> Result<Self> {
        if !(16..=256).contains(&security_key.len()) {
            return Err(NerveError::Protocol(
                "Pairing key is required for video".into(),
            ));
        }
        let std_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| NerveError::Io(e))?;

        let _ = std_socket.set_send_buffer_size(2 * 1024 * 1024);
        let _ = std_socket.set_nonblocking(true);

        let any_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        std_socket
            .bind(&any_addr.into())
            .map_err(|e| NerveError::Io(e))?;

        let socket = UdpSocket::from_std(std_socket.into()).map_err(|e| NerveError::Io(e))?;

        Ok(Self {
            socket,
            target_addr,
            security_key,
        })
    }

    pub async fn send_frame(&self, frame: &EncodedVideoFrame) -> Result<usize> {
        let chunks = VideoFrameChunker::chunk_frame(
            frame.frame_id,
            frame.is_keyframe,
            frame.timestamp_us,
            &frame.data,
        );

        let mut bytes_sent = 0;
        for chunk in chunks {
            let encrypted =
                crate::secure_input::seal_payload(&self.security_key, b"video", &chunk, 2048)
                    .map_err(NerveError::Protocol)?;
            let sent = self
                .socket
                .send_to(&encrypted, self.target_addr)
                .await
                .map_err(|e| NerveError::Io(e))?;
            bytes_sent += sent;
        }

        trace!(
            "Sent video frame {}: {} bytes over UDP to {}",
            frame.frame_id,
            bytes_sent,
            self.target_addr
        );
        Ok(bytes_sent)
    }

    pub fn target_addr(&self) -> SocketAddr {
        self.target_addr
    }
}

pub struct VideoReceiver {
    socket: UdpSocket,
    reassembler: VideoFrameReassembler,
    paired_peer_ip: Arc<Mutex<Option<String>>>,
    security_key: Arc<Mutex<Option<String>>>,
    replay: crate::secure_input::ReplayGuard,
}

impl VideoReceiver {
    pub async fn bind(
        bind_addr: SocketAddr,
        paired_peer_ip: Arc<Mutex<Option<String>>>,
        security_key: Arc<Mutex<Option<String>>>,
    ) -> Result<Self> {
        let std_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| NerveError::Io(e))?;

        let _ = std_socket.set_recv_buffer_size(4 * 1024 * 1024);
        let _ = std_socket.set_nonblocking(true);

        std_socket
            .bind(&bind_addr.into())
            .map_err(|e| NerveError::Io(e))?;

        let socket = UdpSocket::from_std(std_socket.into()).map_err(|e| NerveError::Io(e))?;

        Ok(Self {
            socket,
            reassembler: VideoFrameReassembler::new(),
            paired_peer_ip,
            security_key,
            replay: crate::secure_input::ReplayGuard::new(),
        })
    }

    pub async fn recv_frame(&mut self) -> Result<(u64, bool, u64, Vec<u8>)> {
        let mut buf =
            vec![0u8; MAX_VIDEO_PAYLOAD_SIZE + std::mem::size_of::<VideoChunkHeader>() + 64];

        loop {
            let (len, peer) = self
                .socket
                .recv_from(&mut buf)
                .await
                .map_err(|e| NerveError::Io(e))?;
            let paired = self.paired_peer_ip.lock().ok().and_then(|ip| ip.clone());
            if paired.as_deref() != Some(peer.ip().to_string().as_str()) {
                continue;
            }
            let key = self.security_key.lock().ok().and_then(|key| key.clone());
            let Some(key) = key else { continue };
            let plaintext = match self.replay.open_payload(&key, b"video", &buf[..len], 2048) {
                Ok(plaintext) => plaintext,
                Err(_) => continue,
            };

            match self.reassembler.feed_packet(&plaintext) {
                Ok(Some(completed_frame)) => {
                    return Ok(completed_frame);
                }
                Ok(None) => {
                    continue;
                }
                Err(e) => {
                    trace!("Video packet processing error: {:?}", e);
                    continue;
                }
            }
        }
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}
