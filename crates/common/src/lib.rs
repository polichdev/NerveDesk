pub mod error;
pub mod protocol;

pub use error::{NerveError, Result};
pub use protocol::{
    decode_packet, encode_packet, ButtonState, InputEvent, KeyState, KeyboardPayload,
    MouseButton, MouseButtonPayload, MouseMovePayload, MouseWheelPayload, PacketHeader,
    PacketType, PingPayload, VideoChunkHeader, MAX_PACKET_SIZE, MAX_VIDEO_PAYLOAD_SIZE,
    PROTOCOL_MAGIC, PROTOCOL_VERSION, VIDEO_PROTOCOL_MAGIC,
};
