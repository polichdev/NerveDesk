use std::io::{Cursor, Read, Write};
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};

const JPEG_MAGIC: [u8; 4] = *b"NDM2";
const H264_MAGIC: [u8; 4] = *b"NDH1";
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const HEADER_LEN: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Codec {
    Jpeg,
    H264,
}

impl Codec {
    pub fn port(self) -> u16 {
        match self {
            Self::Jpeg => 48104,
            Self::H264 => 48105,
        }
    }
    fn magic(self) -> [u8; 4] {
        match self {
            Self::Jpeg => JPEG_MAGIC,
            Self::H264 => H264_MAGIC,
        }
    }
}

pub struct MirrorSender {
    pub stream: std::net::TcpStream,
    pub codec: Codec,
    next_id: u64,
    security_key: String,
    inbound: Vec<u8>,
    feedback_replay: crate::secure_input::ReplayGuard,
}

impl MirrorSender {
    pub async fn connect(target: SocketAddr, security_key: String) -> Result<Self, String> {
        if !(16..=256).contains(&security_key.len()) {
            return Err("Pairing key is required for screen streaming".into());
        }
        let h264_target = SocketAddr::new(target.ip(), Codec::H264.port());
        if let Ok(Ok(mut stream)) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            TcpStream::connect(h264_target),
        )
        .await
        {
            let mut ready = [0u8; 1];
            if matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    tokio::io::AsyncReadExt::read_exact(&mut stream, &mut ready)
                )
                .await,
                Ok(Ok(_))
            ) && ready[0] == 0x48
            {
                return Self::configure(stream, Codec::H264, security_key);
            }
        }
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            TcpStream::connect(target),
        )
        .await
        .map_err(|_| format!("Receiver did not respond on TCP {}", target.port()))?
        .map_err(|e| format!("Cannot connect to receiver on TCP {}: {e}", target.port()))?;
        Self::configure(stream, Codec::Jpeg, security_key)
    }

    fn configure(stream: TcpStream, codec: Codec, security_key: String) -> Result<Self, String> {
        let stream = stream.into_std().map_err(|e| e.to_string())?;
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        let _ = socket2::SockRef::from(&stream).set_send_buffer_size(128 * 1024);
        let _ = socket2::SockRef::from(&stream).set_recv_buffer_size(64 * 1024);
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(1)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(3)))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stream,
            codec,
            next_id: 1,
            security_key,
            inbound: Vec::new(),
            feedback_replay: crate::secure_input::ReplayGuard::new(),
        })
    }

    pub fn fallback_to_jpeg(&mut self, target: SocketAddr) -> Result<(), String> {
        let stream =
            std::net::TcpStream::connect_timeout(&target, std::time::Duration::from_secs(3))
                .map_err(|e| e.to_string())?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(1)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(3)))
            .map_err(|e| e.to_string())?;
        self.stream = stream;
        self.codec = Codec::Jpeg;
        self.next_id = 1;
        self.inbound.clear();
        self.feedback_replay = crate::secure_input::ReplayGuard::new();
        Ok(())
    }

    pub fn read_inbound_messages(
        &mut self,
        mut on_event: impl FnMut(&common::InputEvent),
    ) -> Result<bool, String> {
        let mut buf = [0u8; 2048];
        let mut got_ack = false;
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => return Err("Receiver disconnected".into()),
                Ok(n) => {
                    self.inbound.extend_from_slice(&buf[..n]);
                    let mut cursor = 0;
                    while cursor < self.inbound.len() {
                        match self.inbound[cursor] {
                            0x06 => {
                                got_ack = true;
                                cursor += 1;
                            }
                            0xA1 => {
                                if cursor + 3 > self.inbound.len() {
                                    break;
                                }
                                let len = u16::from_le_bytes([
                                    self.inbound[cursor + 1],
                                    self.inbound[cursor + 2],
                                ]) as usize;
                                if len > 2048 + 28 {
                                    return Err("Invalid mirror feedback length".into());
                                }
                                if cursor + 3 + len > self.inbound.len() {
                                    break;
                                }
                                let payload = &self.inbound[cursor + 3..cursor + 3 + len];
                                if let Ok(plain) = self.feedback_replay.open_payload(
                                    &self.security_key,
                                    b"mirror-feedback",
                                    payload,
                                    2048,
                                ) {
                                    if plain.len() >= 3 && plain[0] == 0x02 {
                                        let size =
                                            u16::from_le_bytes([plain[1], plain[2]]) as usize;
                                        if plain.len() == size + 3 {
                                            if let Ok((_header, event)) =
                                                common::decode_packet(&plain[3..])
                                            {
                                                on_event(&event);
                                            }
                                        }
                                    }
                                }
                                cursor += 3 + len;
                            }
                            _ => {
                                cursor += 1;
                            }
                        }
                    }
                    self.inbound.drain(..cursor);
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(got_ack)
    }

    pub fn send(
        &mut self,
        width: u32,
        height: u32,
        offset_x: i16,
        offset_y: i16,
        payload: &[u8],
    ) -> Result<usize, String> {
        if width == 0
            || height == 0
            || width > 7680
            || height > 4320
            || payload.is_empty()
            || payload.len() > MAX_FRAME_BYTES
        {
            return Err("Invalid mirror frame".into());
        }
        let header = make_header(
            self.codec,
            width,
            height,
            offset_x,
            offset_y,
            payload.len() + 28,
            (self.next_id & 0xffffffff) as u32,
        );
        let encrypted = crate::secure_input::seal_payload(
            &self.security_key,
            &header,
            payload,
            MAX_FRAME_BYTES,
        )?;
        self.stream.write_all(&header).map_err(|e| e.to_string())?;
        self.stream
            .write_all(&encrypted)
            .map_err(|e| e.to_string())?;
        self.next_id += 1;
        Ok(header.len() + encrypted.len())
    }
}

pub struct MirrorReceiver {
    listener: TcpListener,
}

impl MirrorReceiver {
    pub async fn bind(codec: Codec) -> Result<Self, String> {
        let listener = TcpListener::bind(("0.0.0.0", codec.port()))
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self { listener })
    }

    pub async fn accept(&self) -> Result<(TcpStream, SocketAddr), String> {
        let (stream, peer_addr) = self.listener.accept().await.map_err(|e| e.to_string())?;
        let _ = socket2::SockRef::from(&stream).set_recv_buffer_size(128 * 1024);
        let _ = socket2::SockRef::from(&stream).set_send_buffer_size(64 * 1024);
        let _ = stream.set_nodelay(true);
        Ok((stream, peer_addr))
    }

    pub async fn read_frame<R: tokio::io::AsyncReadExt + Unpin>(
        stream: &mut R,
        codec: Codec,
        key: &str,
        replay: &mut crate::secure_input::ReplayGuard,
        last_id: &mut u32,
    ) -> Result<(u32, u32, i16, i16, Vec<u8>), String> {
        let mut header = [0u8; HEADER_LEN];
        stream
            .read_exact(&mut header)
            .await
            .map_err(|e| e.to_string())?;
        let (width, height, offset_x, offset_y, len) = parse_header(codec, &header)?;
        let id = u32::from_le_bytes(header[16..20].try_into().unwrap());
        if id <= *last_id {
            return Err("Repeated mirror frame".into());
        }
        let mut payload = vec![0u8; len];
        stream
            .read_exact(&mut payload)
            .await
            .map_err(|e| e.to_string())?;
        let payload = replay.open_payload(key, &header, &payload, MAX_FRAME_BYTES)?;
        *last_id = id;
        Ok((width, height, offset_x, offset_y, payload))
    }
}

fn make_header(
    codec: Codec,
    width: u32,
    height: u32,
    offset_x: i16,
    offset_y: i16,
    len: usize,
    id: u32,
) -> [u8; HEADER_LEN] {
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(&codec.magic());
    header[4..6].copy_from_slice(&(width as u16).to_le_bytes());
    header[6..8].copy_from_slice(&(height as u16).to_le_bytes());
    header[8..12].copy_from_slice(&(len as u32).to_le_bytes());
    header[12..14].copy_from_slice(&offset_x.to_le_bytes());
    header[14..16].copy_from_slice(&offset_y.to_le_bytes());
    header[16..20].copy_from_slice(&id.to_le_bytes());
    header
}

fn parse_header(
    codec: Codec,
    header: &[u8; HEADER_LEN],
) -> Result<(u32, u32, i16, i16, usize), String> {
    let width = u16::from_le_bytes(header[4..6].try_into().unwrap()) as u32;
    let height = u16::from_le_bytes(header[6..8].try_into().unwrap()) as u32;
    let len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    let offset_x = i16::from_le_bytes(header[12..14].try_into().unwrap());
    let offset_y = i16::from_le_bytes(header[14..16].try_into().unwrap());
    if header[..4] != codec.magic()
        || width == 0
        || height == 0
        || width > 7680
        || height > 4320
        || len == 0
        || len > MAX_FRAME_BYTES + 28
    {
        return Err("Invalid mirror frame header".into());
    }
    Ok((width, height, offset_x, offset_y, len))
}

pub fn encode_bgra(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>, String> {
    if width == 0
        || height == 0
        || width > 7680
        || height > 4320
        || bgra.len() != (width as usize) * (height as usize) * 4
    {
        return Err("Invalid BGRA frame".into());
    }
    let mut jpeg = Vec::with_capacity((width * height / 4) as usize);
    let mut encoder = jpeg_encoder::Encoder::new(&mut jpeg, 65);
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0);
    encoder
        .encode(
            bgra,
            width as u16,
            height as u16,
            jpeg_encoder::ColorType::Bgra,
        )
        .map_err(|e| e.to_string())?;
    Ok(jpeg)
}

pub fn decode_bgra(width: u32, height: u32, jpeg: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(jpeg));
    decoder.set_max_decoding_buffer_size(7680 * 4320 * 3);
    let rgb = decoder.decode().map_err(|e| e.to_string())?;
    let info = decoder.info().ok_or("Missing JPEG dimensions")?;
    if info.width as u32 != width
        || info.height as u32 != height
        || info.pixel_format != jpeg_decoder::PixelFormat::RGB24
        || rgb.len() != (width as usize) * (height as usize) * 3
    {
        return Err("Unexpected JPEG frame format".into());
    }
    let mut bgra = vec![0u8; (width as usize) * (height as usize) * 4];
    for (src, dst) in rgb.chunks_exact(3).zip(bgra.chunks_exact_mut(4)) {
        dst[0] = src[2];
        dst[1] = src[1];
        dst[2] = src[0];
        dst[3] = 255;
    }
    Ok(bgra)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_roundtrip_preserves_size_and_color_order() {
        let mut bgra = vec![0u8; 32 * 32 * 4];
        for pixel in bgra.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[10, 80, 200, 255]);
        }
        let jpeg = encode_bgra(32, 32, &bgra).unwrap();
        let result = decode_bgra(32, 32, &jpeg).unwrap();
        assert_eq!(result.len(), bgra.len());
        assert!((result[0] as i32 - 10).abs() < 10);
        assert!((result[1] as i32 - 80).abs() < 10);
        assert!((result[2] as i32 - 200).abs() < 10);
    }

    #[test]
    fn framed_mirror_header_roundtrip() {
        let header = make_header(Codec::Jpeg, 320, 240, 50, -20, 1024, 12);
        assert_eq!(
            parse_header(Codec::Jpeg, &header).unwrap(),
            (320, 240, 50, -20, 1024)
        );
        let mut bad = header;
        bad[0] = 0;
        assert!(parse_header(Codec::Jpeg, &bad).is_err());
        assert!(parse_header(Codec::H264, &header).is_err());
    }
}
