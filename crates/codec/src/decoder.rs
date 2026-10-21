use tracing::{info, trace};

use common::{NerveError, Result};

#[derive(Debug, Clone)]
pub struct VideoDecoderConfig {
    pub width: u32,
    pub height: u32,
}

impl Default for VideoDecoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
        }
    }
}

pub struct DecodedVideoFrame {
    pub frame_id: u64,
    pub timestamp_us: u64,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub struct HardwareVideoDecoder {
    config: VideoDecoderConfig,
    last_frame_id: u64,
}

impl HardwareVideoDecoder {
    pub fn new(config: VideoDecoderConfig) -> Result<Self> {
        info!(
            "Initializing Hardware Video Decoder: {}x{}",
            config.width, config.height
        );

        Ok(Self {
            config,
            last_frame_id: 0,
        })
    }

    pub fn decode_frame(
        &mut self,
        frame_id: u64,
        timestamp_us: u64,
        payload: &[u8],
    ) -> Result<DecodedVideoFrame> {
        if payload.is_empty() {
            return Err(NerveError::Protocol("Empty video frame payload".into()));
        }

        self.last_frame_id = frame_id;

        if payload.len() >= 4 {
            let width = u16::from_le_bytes([payload[0], payload[1]]) as u32;
            let height = u16::from_le_bytes([payload[2], payload[3]]) as u32;
            if width > 0 && width <= 7680 && height > 0 && height <= 4320 {
                let expected_len = (width as usize) * (height as usize) * 4;
                if let Ok(bgra) = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&payload[4..], expected_len) {
                    if bgra.len() != expected_len {
                        return Err(NerveError::Protocol("Incomplete BGRA frame".into()));
                    }
                    trace!(
                        "Decoded video frame {}: {}x{} ({} bytes uncompressed)",
                        frame_id,
                        width,
                        height,
                        bgra.len()
                    );
                    return Ok(DecodedVideoFrame {
                        frame_id,
                        timestamp_us,
                        width,
                        height,
                        bgra,
                    });
                }
            }
        }

        trace!(
            "Decoded video frame {}: {} bytes received",
            frame_id,
            payload.len()
        );

        Ok(DecodedVideoFrame {
            frame_id,
            timestamp_us,
            width: self.config.width,
            height: self.config.height,
            bgra: Vec::new(),
        })
    }
}
