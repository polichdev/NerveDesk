use std::net::SocketAddr;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tracing::{debug, trace, warn};

use codec::{EncodedVideoFrame, VideoFrameChunker, VideoFrameReassembler};
use common::{NerveError, Result, VideoChunkHeader, MAX_VIDEO_PAYLOAD_SIZE};

pub struct VideoSender {
    socket: UdpSocket,
    target_addr: SocketAddr,
}

impl VideoSender {
    pub async fn new(target_addr: SocketAddr) -> Result<Self> {
        let std_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| NerveError::Io(e))?;

        let _ = std_socket.set_send_buffer_size(2 * 1024 * 1024);
        let _ = std_socket.set_nonblocking(true);

        let any_addr: SocketAddr = "0.0.0.0:0".parse().unwrap();
        std_socket
            .bind(&any_addr.into())
            .map_err(|e| NerveError::Io(e))?;

        let socket = UdpSocket::from_std(std_socket.into())
            .map_err(|e| NerveError::Io(e))?;

        Ok(Self {
            socket,
            target_addr,
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
            let sent = self
                .socket
                .send_to(&chunk, self.target_addr)
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
}

impl VideoReceiver {
    pub async fn bind(bind_addr: SocketAddr) -> Result<Self> {
        let std_socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
            .map_err(|e| NerveError::Io(e))?;

        let _ = std_socket.set_recv_buffer_size(4 * 1024 * 1024);
        let _ = std_socket.set_nonblocking(true);

        std_socket
            .bind(&bind_addr.into())
            .map_err(|e| NerveError::Io(e))?;

        let socket = UdpSocket::from_std(std_socket.into())
            .map_err(|e| NerveError::Io(e))?;

        Ok(Self {
            socket,
            reassembler: VideoFrameReassembler::new(),
        })
    }

    pub async fn recv_frame(&mut self) -> Result<(u64, bool, u64, Vec<u8>)> {
        let mut buf = vec![0u8; MAX_VIDEO_PAYLOAD_SIZE + std::mem::size_of::<VideoChunkHeader>() + 64];

        loop {
            let (len, _peer) = self
                .socket
                .recv_from(&mut buf)
                .await
                .map_err(|e| NerveError::Io(e))?;

            match self.reassembler.feed_packet(&buf[..len]) {
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
