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

        trace!(
            "Decoded video frame {}: {} bytes received",
            frame_id,
            payload.len()
        );
        self.last_frame_id = frame_id;

        Ok(DecodedVideoFrame {
            frame_id,
            timestamp_us,
            width: self.config.width,
            height: self.config.height,
        })
    }
}
