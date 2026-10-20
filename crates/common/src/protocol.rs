use bytemuck::{Pod, Zeroable};
use crate::error::{NerveError, Result};

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

    pub fn validate(&self) -> Result<()> {
        if self.magic != VIDEO_PROTOCOL_MAGIC {
            return Err(NerveError::Protocol("Invalid video packet magic".into()));
        }
        if self.version != PROTOCOL_VERSION {
            return Err(NerveError::Protocol("Unsupported video protocol version".into()));
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
            other => Err(NerveError::Protocol(format!("Unknown packet type: {other}"))),
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
            other => Err(NerveError::Protocol(format!("Unknown mouse button: {other}"))),
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
            other => Err(NerveError::Protocol(format!("Unknown button state: {other}"))),
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

#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    MouseMove {
        dx: i32,
        dy: i32,
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
    Heartbeat,
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
                return Err(NerveError::Protocol("Buffer too small for MouseMove".into()));
            }
            let header = PacketHeader::new(PacketType::MouseMove, sequence, timestamp_us);
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
                return Err(NerveError::Protocol("Buffer too small for MouseButton".into()));
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
                return Err(NerveError::Protocol("Buffer too small for MouseWheel".into()));
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
        InputEvent::Ping { client_timestamp_us } => {
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
        InputEvent::Pong { client_timestamp_us } => {
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
        InputEvent::Heartbeat => {
            let total = header_size;
            if buf.len() < total {
                return Err(NerveError::Protocol("Buffer too small for Heartbeat".into()));
            }
            let header = PacketHeader::new(PacketType::Heartbeat, sequence, timestamp_us);
            buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            Ok(total)
        }
    }
}

pub fn decode_packet(buf: &[u8]) -> Result<(PacketHeader, InputEvent)> {
    let header_size = std::mem::size_of::<PacketHeader>();
    if buf.len() < header_size {
        return Err(NerveError::Protocol("Buffer smaller than packet header".into()));
    }

    let header: PacketHeader = *bytemuck::from_bytes(&buf[..header_size]);
    let packet_type = header.validate()?;

    let payload_bytes = &buf[header_size..];

    let event = match packet_type {
        PacketType::MouseMove => {
            let payload_size = std::mem::size_of::<MouseMovePayload>();
            if payload_bytes.len() < payload_size {
                return Err(NerveError::Protocol("Truncated MouseMove payload".into()));
            }
            let payload: MouseMovePayload = *bytemuck::from_bytes(&payload_bytes[..payload_size]);
            InputEvent::MouseMove {
                dx: payload.dx,
                dy: payload.dy,
            }
        }
        PacketType::MouseButton => {
            let payload_size = std::mem::size_of::<MouseButtonPayload>();
            if payload_bytes.len() < payload_size {
                return Err(NerveError::Protocol("Truncated MouseButton payload".into()));
            }
            let payload: MouseButtonPayload = *bytemuck::from_bytes(&payload_bytes[..payload_size]);
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
            let payload: MouseWheelPayload = *bytemuck::from_bytes(&payload_bytes[..payload_size]);
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
            let payload: KeyboardPayload = *bytemuck::from_bytes(&payload_bytes[..payload_size]);
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
            let payload: PingPayload = *bytemuck::from_bytes(&payload_bytes[..payload_size]);
            InputEvent::Ping {
                client_timestamp_us: payload.client_timestamp_us,
            }
        }
        PacketType::Pong => {
            let payload_size = std::mem::size_of::<PingPayload>();
            if payload_bytes.len() < payload_size {
                return Err(NerveError::Protocol("Truncated Pong payload".into()));
            }
            let payload: PingPayload = *bytemuck::from_bytes(&payload_bytes[..payload_size]);
            InputEvent::Pong {
                client_timestamp_us: payload.client_timestamp_us,
            }
        }
        PacketType::Heartbeat => InputEvent::Heartbeat,
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
}
