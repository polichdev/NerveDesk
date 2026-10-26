use crate::error::{NerveError, Result};
use bytemuck::{Pod, Zeroable};

pub const PROTOCOL_MAGIC: [u8; 4] = *b"NRVE";
pub const VIDEO_PROTOCOL_MAGIC: [u8; 4] = *b"NRVV";
pub const PROTOCOL_VERSION: u8 = 1;

pub const MAX_VIDEO_PAYLOAD_SIZE: usize = 1350;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct VideoChunkHeader {
    pub magic: [u8; 4],
    pub version: u8,
    pub flags: u8,
    pub chunk_index: u16,
    pub total_chunks: u16,
    pub payload_size: u16,
    pub _reserved: [u8; 4],
    pub frame_id: u64,
    pub timestamp_us: u64,
}

impl VideoChunkHeader {
    pub fn new(
        frame_id: u64,
        chunk_index: u16,
        total_chunks: u16,
        payload_size: u16,
        is_keyframe: bool,
        is_last_chunk: bool,
        timestamp_us: u64,
    ) -> Self {
        let mut flags = 0u8;
        if is_keyframe {
            flags |= 1 << 0;
        }
        if is_last_chunk {
            flags |= 1 << 1;
        }

        Self {
            magic: VIDEO_PROTOCOL_MAGIC,
            version: PROTOCOL_VERSION,
            flags,
            chunk_index,
            total_chunks,
            payload_size,
            _reserved: [0; 4],
            frame_id,
            timestamp_us,
        }
    }

    pub fn is_keyframe(&self) -> bool {
        (self.flags & (1 << 0)) != 0
    }

    pub fn is_last_chunk(&self) -> bool {
        (self.flags & (1 << 1)) != 0
    }

    pub fn new_fec(
        frame_id: u64,
        group_index: u16,
        total_chunks: u16,
        payload_size: u16,
        start_chunk: u16,
        chunk_count: u16,
        timestamp_us: u64,
    ) -> Self {
        let mut reserved = [0u8; 4];
        reserved[0..2].copy_from_slice(&start_chunk.to_le_bytes());
        reserved[2..4].copy_from_slice(&chunk_count.to_le_bytes());
        Self {
            magic: VIDEO_PROTOCOL_MAGIC,
            version: PROTOCOL_VERSION,
            flags: 1 << 2,
            chunk_index: group_index,
            total_chunks,
            payload_size,
            _reserved: reserved,
            frame_id,
            timestamp_us,
        }
    }

    pub fn is_fec_parity(&self) -> bool {
        (self.flags & (1 << 2)) != 0
    }

    pub fn fec_group_range(&self) -> (u16, u16) {
        let start = u16::from_le_bytes([self._reserved[0], self._reserved[1]]);
        let count = u16::from_le_bytes([self._reserved[2], self._reserved[3]]);
        (start, count)
    }

    pub fn validate(&self) -> Result<()> {
        if self.magic != VIDEO_PROTOCOL_MAGIC {
            return Err(NerveError::Protocol("Invalid video packet magic".into()));
        }
        if self.version != PROTOCOL_VERSION {
            return Err(NerveError::Protocol(
                "Unsupported video protocol version".into(),
            ));
        }
        Ok(())
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketType {
    MouseMove = 1,
    MouseButton = 2,
    MouseWheel = 3,
    Keyboard = 4,
    Ping = 5,
    Pong = 6,
    Heartbeat = 7,
    CursorPosition = 8,
    FileDrag = 9,
}

impl TryFrom<u8> for PacketType {
    type Error = NerveError;

    fn try_from(val: u8) -> Result<Self> {
        match val {
            1 => Ok(Self::MouseMove),
            2 => Ok(Self::MouseButton),
            3 => Ok(Self::MouseWheel),
            4 => Ok(Self::Keyboard),
            5 => Ok(Self::Ping),
            6 => Ok(Self::Pong),
            7 => Ok(Self::Heartbeat),
            8 => Ok(Self::CursorPosition),
            9 => Ok(Self::FileDrag),
            other => Err(NerveError::Protocol(format!(
                "Unknown packet type: {other}"
            ))),
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct PacketHeader {
    pub magic: [u8; 4],
    pub version: u8,
    pub packet_type: u8,
    pub _reserved: [u8; 2],
    pub sequence: u64,
    pub timestamp_us: u64,
}

impl PacketHeader {
    pub fn new(packet_type: PacketType, sequence: u64, timestamp_us: u64) -> Self {
        Self {
            magic: PROTOCOL_MAGIC,
            version: PROTOCOL_VERSION,
            packet_type: packet_type as u8,
            _reserved: [0; 2],
            sequence,
            timestamp_us,
        }
    }

    pub fn validate(&self) -> Result<PacketType> {
        if self.magic != PROTOCOL_MAGIC {
            return Err(NerveError::Protocol(format!(
                "Invalid protocol magic: {:?}",
                self.magic
            )));
        }
        if self.version != PROTOCOL_VERSION {
            return Err(NerveError::Protocol(format!(
                "Unsupported protocol version: {}",
                self.version
            )));
        }
        PacketType::try_from(self.packet_type)
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct MouseMovePayload {
    pub dx: i32,
    pub dy: i32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct CursorPositionPayload {
    pub x: i32,
    pub y: i32,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left = 0,
    Right = 1,
    Middle = 2,
    X1 = 3,
    X2 = 4,
}

impl TryFrom<u8> for MouseButton {
    type Error = NerveError;
    fn try_from(val: u8) -> Result<Self> {
        match val {
            0 => Ok(Self::Left),
            1 => Ok(Self::Right),
            2 => Ok(Self::Middle),
            3 => Ok(Self::X1),
            4 => Ok(Self::X2),
            other => Err(NerveError::Protocol(format!(
                "Unknown mouse button: {other}"
            ))),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Released = 0,
    Pressed = 1,
}

impl TryFrom<u8> for ButtonState {
    type Error = NerveError;
    fn try_from(val: u8) -> Result<Self> {
        match val {
            0 => Ok(Self::Released),
            1 => Ok(Self::Pressed),
            other => Err(NerveError::Protocol(format!(
                "Unknown button state: {other}"
            ))),
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct MouseButtonPayload {
    pub button: u8,
    pub state: u8,
    pub _padding: [u8; 2],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct MouseWheelPayload {
    pub delta: i16,
    pub horizontal: u8,
    pub _padding: u8,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    KeyUp = 0,
    KeyDown = 1,
}

impl TryFrom<u8> for KeyState {
    type Error = NerveError;
    fn try_from(val: u8) -> Result<Self> {
        match val {
            0 => Ok(Self::KeyUp),
            1 => Ok(Self::KeyDown),
            other => Err(NerveError::Protocol(format!("Unknown key state: {other}"))),
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct KeyboardPayload {
    pub vk_code: u16,
    pub scan_code: u16,
    pub state: u8,
    pub is_extended: u8,
    pub _padding: [u8; 2],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct PingPayload {
    pub client_timestamp_us: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct HeartbeatPayload {
    pub device_name: [u8; 32],
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct FileDragPayload {
    pub active: u8,
    pub _padding: [u8; 3],
    pub file_name: [u8; 32],
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    MouseMove {
        dx: i32,
        dy: i32,
    },
    CursorPosition {
        x: i32,
        y: i32,
    },
    MouseButton {
        button: MouseButton,
        state: ButtonState,
    },
    MouseWheel {
        delta: i16,
        horizontal: bool,
    },
    Keyboard {
        vk_code: u16,
        scan_code: u16,
        state: KeyState,
        is_extended: bool,
    },
    Ping {
        client_timestamp_us: u64,
    },
    Pong {
        client_timestamp_us: u64,
    },
    Heartbeat {
        device_name: String,
    },
    FileDrag {
        active: bool,
        file_name: String,
    },
}

pub const MAX_PACKET_SIZE: usize = 64;

pub fn encode_packet(
    event: &InputEvent,
    sequence: u64,
    timestamp_us: u64,
    buf: &mut [u8],
) -> Result<usize> {
    let header_size = std::mem::size_of::<PacketHeader>();

    match event {
        InputEvent::MouseMove { dx, dy } => {
            let payload = MouseMovePayload {
                dx: *dx,
                dy: *dy,
                flags: 0,
            };
            let payload_size = std::mem::size_of::<MouseMovePayload>();
            let total = header_size + payload_size;
            if buf.len() < total {
                return Err(NerveError::Protocol(
                    "Buffer too small for MouseMove".into(),
                ));
            }
            let header = PacketHeader::new(PacketType::MouseMove, sequence, timestamp_us);
            buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            buf[header_size..total].copy_from_slice(bytemuck::bytes_of(&payload));
            Ok(total)
        }
        InputEvent::CursorPosition { x, y } => {
            let payload = CursorPositionPayload { x: *x, y: *y };
            let payload_size = std::mem::size_of::<CursorPositionPayload>();
            let total = header_size + payload_size;
            if buf.len() < total {
                return Err(NerveError::Protocol(
                    "Buffer too small for CursorPosition".into(),
                ));
            }
            let header = PacketHeader::new(PacketType::CursorPosition, sequence, timestamp_us);
            buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            buf[header_size..total].copy_from_slice(bytemuck::bytes_of(&payload));
            Ok(total)
        }
        InputEvent::MouseButton { button, state } => {
            let payload = MouseButtonPayload {
                button: *button as u8,
                state: *state as u8,
                _padding: [0; 2],
            };
            let payload_size = std::mem::size_of::<MouseButtonPayload>();
            let total = header_size + payload_size;
            if buf.len() < total {
                return Err(NerveError::Protocol(
                    "Buffer too small for MouseButton".into(),
                ));
            }
            let header = PacketHeader::new(PacketType::MouseButton, sequence, timestamp_us);
            buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            buf[header_size..total].copy_from_slice(bytemuck::bytes_of(&payload));
            Ok(total)
        }
        InputEvent::MouseWheel { delta, horizontal } => {
            let payload = MouseWheelPayload {
                delta: *delta,
                horizontal: if *horizontal { 1 } else { 0 },
                _padding: 0,
            };
            let payload_size = std::mem::size_of::<MouseWheelPayload>();
            let total = header_size + payload_size;
            if buf.len() < total {
                return Err(NerveError::Protocol(
                    "Buffer too small for MouseWheel".into(),
                ));
            }
            let header = PacketHeader::new(PacketType::MouseWheel, sequence, timestamp_us);
            buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            buf[header_size..total].copy_from_slice(bytemuck::bytes_of(&payload));
            Ok(total)
        }
        InputEvent::Keyboard {
            vk_code,
            scan_code,
            state,
            is_extended,
        } => {
            let payload = KeyboardPayload {
                vk_code: *vk_code,
                scan_code: *scan_code,
                state: *state as u8,
                is_extended: if *is_extended { 1 } else { 0 },
                _padding: [0; 2],
            };
            let payload_size = std::mem::size_of::<KeyboardPayload>();
            let total = header_size + payload_size;
            if buf.len() < total {
                return Err(NerveError::Protocol("Buffer too small for Keyboard".into()));
            }
            let header = PacketHeader::new(PacketType::Keyboard, sequence, timestamp_us);
            buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            buf[header_size..total].copy_from_slice(bytemuck::bytes_of(&payload));
            Ok(total)
        }
        InputEvent::Ping {
            client_timestamp_us,
        } => {
            let payload = PingPayload {
                client_timestamp_us: *client_timestamp_us,
            };
            let payload_size = std::mem::size_of::<PingPayload>();
            let total = header_size + payload_size;
            if buf.len() < total {
                return Err(NerveError::Protocol("Buffer too small for Ping".into()));
            }
            let header = PacketHeader::new(PacketType::Ping, sequence, timestamp_us);
            buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            buf[header_size..total].copy_from_slice(bytemuck::bytes_of(&payload));
            Ok(total)
        }
        InputEvent::Pong {
            client_timestamp_us,
        } => {
            let payload = PingPayload {
                client_timestamp_us: *client_timestamp_us,
            };
            let payload_size = std::mem::size_of::<PingPayload>();
            let total = header_size + payload_size;
            if buf.len() < total {
                return Err(NerveError::Protocol("Buffer too small for Pong".into()));
            }
            let header = PacketHeader::new(PacketType::Pong, sequence, timestamp_us);
            buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            buf[header_size..total].copy_from_slice(bytemuck::bytes_of(&payload));
            Ok(total)
        }
        InputEvent::Heartbeat { device_name } => {
            let mut payload = HeartbeatPayload {
                device_name: [0; 32],
            };
            let bytes = device_name.as_bytes();
            let len = bytes.len().min(32);
            payload.device_name[..len].copy_from_slice(&bytes[..len]);
            let payload_size = std::mem::size_of::<HeartbeatPayload>();
            let total = header_size + payload_size;
            if buf.len() < total {
                return Err(NerveError::Protocol(
                    "Buffer too small for Heartbeat".into(),
                ));
            }
            let header = PacketHeader::new(PacketType::Heartbeat, sequence, timestamp_us);
            buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            buf[header_size..total].copy_from_slice(bytemuck::bytes_of(&payload));
            Ok(total)
        }
        InputEvent::FileDrag { active, file_name } => {
            let mut payload = FileDragPayload {
                active: if *active { 1 } else { 0 },
                _padding: [0; 3],
                file_name: [0; 32],
            };
            let bytes = file_name.as_bytes();
            let len = bytes.len().min(32);
            payload.file_name[..len].copy_from_slice(&bytes[..len]);
            let payload_size = std::mem::size_of::<FileDragPayload>();
            let total = header_size + payload_size;
            if buf.len() < total {
                return Err(NerveError::Protocol("Buffer too small for FileDrag".into()));
            }
            let header = PacketHeader::new(PacketType::FileDrag, sequence, timestamp_us);
            buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            buf[header_size..total].copy_from_slice(bytemuck::bytes_of(&payload));
            Ok(total)
        }
    }
}

pub fn decode_packet(buf: &[u8]) -> Result<(PacketHeader, InputEvent)> {
    let header_size = std::mem::size_of::<PacketHeader>();
    if buf.len() < header_size {
        return Err(NerveError::Protocol(
            "Buffer smaller than packet header".into(),
        ));
    }

    let header: PacketHeader = bytemuck::pod_read_unaligned(&buf[..header_size]);
    let packet_type = header.validate()?;

    let payload_bytes = &buf[header_size..];

    let event = match packet_type {
        PacketType::MouseMove => {
            let payload_size = std::mem::size_of::<MouseMovePayload>();
            if payload_bytes.len() < payload_size {
                return Err(NerveError::Protocol("Truncated MouseMove payload".into()));
            }
            let payload: MouseMovePayload =
                bytemuck::pod_read_unaligned(&payload_bytes[..payload_size]);
            InputEvent::MouseMove {
                dx: payload.dx,
                dy: payload.dy,
            }
        }
        PacketType::CursorPosition => {
            let payload_size = std::mem::size_of::<CursorPositionPayload>();
            if payload_bytes.len() < payload_size {
                return Err(NerveError::Protocol(
                    "Truncated CursorPosition payload".into(),
                ));
            }
            let payload: CursorPositionPayload =
                bytemuck::pod_read_unaligned(&payload_bytes[..payload_size]);
            InputEvent::CursorPosition {
                x: payload.x,
                y: payload.y,
            }
        }
        PacketType::MouseButton => {
            let payload_size = std::mem::size_of::<MouseButtonPayload>();
            if payload_bytes.len() < payload_size {
                return Err(NerveError::Protocol("Truncated MouseButton payload".into()));
            }
            let payload: MouseButtonPayload =
                bytemuck::pod_read_unaligned(&payload_bytes[..payload_size]);
            InputEvent::MouseButton {
                button: MouseButton::try_from(payload.button)?,
                state: ButtonState::try_from(payload.state)?,
            }
        }
        PacketType::MouseWheel => {
            let payload_size = std::mem::size_of::<MouseWheelPayload>();
            if payload_bytes.len() < payload_size {
                return Err(NerveError::Protocol("Truncated MouseWheel payload".into()));
            }
            let payload: MouseWheelPayload =
                bytemuck::pod_read_unaligned(&payload_bytes[..payload_size]);
            InputEvent::MouseWheel {
                delta: payload.delta,
                horizontal: payload.horizontal != 0,
            }
        }
        PacketType::Keyboard => {
            let payload_size = std::mem::size_of::<KeyboardPayload>();
            if payload_bytes.len() < payload_size {
                return Err(NerveError::Protocol("Truncated Keyboard payload".into()));
            }
            let payload: KeyboardPayload =
                bytemuck::pod_read_unaligned(&payload_bytes[..payload_size]);
            InputEvent::Keyboard {
                vk_code: payload.vk_code,
                scan_code: payload.scan_code,
                state: KeyState::try_from(payload.state)?,
                is_extended: payload.is_extended != 0,
            }
        }
        PacketType::Ping => {
            let payload_size = std::mem::size_of::<PingPayload>();
            if payload_bytes.len() < payload_size {
                return Err(NerveError::Protocol("Truncated Ping payload".into()));
            }
            let payload: PingPayload = bytemuck::pod_read_unaligned(&payload_bytes[..payload_size]);
            InputEvent::Ping {
                client_timestamp_us: payload.client_timestamp_us,
            }
        }
        PacketType::Pong => {
            let payload_size = std::mem::size_of::<PingPayload>();
            if payload_bytes.len() < payload_size {
                return Err(NerveError::Protocol("Truncated Pong payload".into()));
            }
            let payload: PingPayload = bytemuck::pod_read_unaligned(&payload_bytes[..payload_size]);
            InputEvent::Pong {
                client_timestamp_us: payload.client_timestamp_us,
            }
        }
        PacketType::Heartbeat => {
            let payload_size = std::mem::size_of::<HeartbeatPayload>();
            if payload_bytes.len() >= payload_size {
                let payload: HeartbeatPayload =
                    bytemuck::pod_read_unaligned(&payload_bytes[..payload_size]);
                let end = payload
                    .device_name
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(32);
                let name = String::from_utf8_lossy(&payload.device_name[..end]).to_string();
                InputEvent::Heartbeat { device_name: name }
            } else {
                InputEvent::Heartbeat {
                    device_name: String::new(),
                }
            }
        }
        PacketType::FileDrag => {
            let payload_size = std::mem::size_of::<FileDragPayload>();
            if payload_bytes.len() >= payload_size {
                let payload: FileDragPayload =
                    bytemuck::pod_read_unaligned(&payload_bytes[..payload_size]);
                let end = payload.file_name.iter().position(|&b| b == 0).unwrap_or(32);
                let name = String::from_utf8_lossy(&payload.file_name[..end]).to_string();
                InputEvent::FileDrag {
                    active: payload.active != 0,
                    file_name: name,
                }
            } else {
                InputEvent::FileDrag {
                    active: false,
                    file_name: String::new(),
                }
            }
        }
    };

    Ok((header, event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mouse_move_roundtrip() {
        let mut buf = [0u8; MAX_PACKET_SIZE];
        let original_event = InputEvent::MouseMove { dx: -42, dy: 100 };
        let seq = 12345;
        let ts = 987654321;

        let len = encode_packet(&original_event, seq, ts, &mut buf).expect("Encode failed");
        assert!(len <= MAX_PACKET_SIZE);

        let (header, decoded_event) = decode_packet(&buf[..len]).expect("Decode failed");
        assert_eq!(header.sequence, seq);
        assert_eq!(header.timestamp_us, ts);
        assert_eq!(decoded_event, original_event);
    }

    #[test]
    fn test_cursor_position_roundtrip() {
        let mut buf = [0u8; MAX_PACKET_SIZE];
        let original_event = InputEvent::CursorPosition { x: 1919, y: 1079 };
        let seq = 999;
        let ts = 123456789;

        let len = encode_packet(&original_event, seq, ts, &mut buf).expect("Encode failed");
        let (header, decoded_event) = decode_packet(&buf[..len]).expect("Decode failed");

        assert_eq!(header.packet_type, PacketType::CursorPosition as u8);
        assert_eq!(decoded_event, original_event);
    }

    #[test]
    fn test_keyboard_roundtrip() {
        let mut buf = [0u8; MAX_PACKET_SIZE];
        let original_event = InputEvent::Keyboard {
            vk_code: 0x41,
            scan_code: 0x1E,
            state: KeyState::KeyDown,
            is_extended: false,
        };

        let len = encode_packet(&original_event, 1, 1000, &mut buf).expect("Encode failed");
        let (header, decoded_event) = decode_packet(&buf[..len]).expect("Decode failed");

        assert_eq!(header.packet_type, PacketType::Keyboard as u8);
        assert_eq!(decoded_event, original_event);
    }

    #[test]
    fn test_heartbeat_roundtrip() {
        let mut buf = [0u8; MAX_PACKET_SIZE];
        let original_event = InputEvent::Heartbeat {
            device_name: "DEVICE-TEST".to_string(),
        };

        let len = encode_packet(&original_event, 5, 2000, &mut buf).expect("Encode failed");
        let (header, decoded_event) = decode_packet(&buf[..len]).expect("Decode failed");

        assert_eq!(header.packet_type, PacketType::Heartbeat as u8);
        assert_eq!(decoded_event, original_event);
    }

    #[test]
    fn test_file_drag_roundtrip() {
        let mut buf = [0u8; MAX_PACKET_SIZE];
        let original_event = InputEvent::FileDrag {
            active: true,
            file_name: "photo.jpg".to_string(),
        };

        let len = encode_packet(&original_event, 7, 3000, &mut buf).expect("Encode failed");
        let (header, decoded_event) = decode_packet(&buf[..len]).expect("Decode failed");

        assert_eq!(header.packet_type, PacketType::FileDrag as u8);
        assert_eq!(decoded_event, original_event);
    }
}
