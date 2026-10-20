use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, trace};

use common::Result;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

#[derive(Debug, Clone)]
pub struct VideoEncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub keyframe_interval: u32,
}

impl Default for VideoEncoderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_bps: 15_000_000,
            keyframe_interval: 120,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncodedVideoFrame {
    pub frame_id: u64,
    pub is_keyframe: bool,
    pub timestamp_us: u64,
    pub data: Vec<u8>,
}

pub struct HardwareVideoEncoder {
    config: VideoEncoderConfig,
    frame_counter: AtomicU64,
}

impl HardwareVideoEncoder {
    pub fn new(config: VideoEncoderConfig) -> Result<Self> {
        info!(
            "Initializing Hardware Video Encoder: {}x{} @ {} FPS, bitrate={} Mbps",
            config.width,
            config.height,
            config.fps,
            config.bitrate_bps / 1_000_000
        );

        Ok(Self {
            config,
            frame_counter: AtomicU64::new(1),
        })
    }

    pub fn encode_frame(
        &self,
        _texture: &ID3D11Texture2D,
        timestamp_us: u64,
        force_keyframe: bool,
    ) -> Result<EncodedVideoFrame> {
        let frame_id = self.frame_counter.fetch_add(1, Ordering::Relaxed);
        let is_keyframe = force_keyframe
            || (frame_id % self.config.keyframe_interval as u64 == 0)
            || (frame_id == 1);

        let mut dummy_compressed = Vec::with_capacity(512);
        if is_keyframe {
            dummy_compressed.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x67]);
            dummy_compressed.extend_from_slice(&[0x42, 0x00, 0x1f]);
            dummy_compressed.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x68]);
            dummy_compressed.extend_from_slice(&[0xce, 0x3c, 0x80]);
            dummy_compressed.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x65]);
        } else {
            dummy_compressed.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x41]);
        }
        dummy_compressed.extend_from_slice(&frame_id.to_le_bytes());

        trace!(
            "Encoded frame {}: keyframe={} bytes={} ts={}",
            frame_id,
            is_keyframe,
            dummy_compressed.len(),
            timestamp_us
        );

        Ok(EncodedVideoFrame {
            frame_id,
            is_keyframe,
            timestamp_us,
            data: dummy_compressed,
        })
    }

    pub fn encode_bgra(
        &self,
        width: u32,
        height: u32,
        bgra: &[u8],
        timestamp_us: u64,
        force_keyframe: bool,
    ) -> Result<EncodedVideoFrame> {
        let frame_id = self.frame_counter.fetch_add(1, Ordering::Relaxed);
        let is_keyframe = force_keyframe
            || (frame_id % self.config.keyframe_interval as u64 == 0)
            || (frame_id == 1);

        let (target_w, target_h, src_data) = if width > 1280 {
            let tw = width / 2;
            let th = height / 2;
            let mut scaled = vec![0u8; (tw * th * 4) as usize];
            for y in 0..th {
                let src_row = (y * 2 * width * 4) as usize;
                let dst_row = (y * tw * 4) as usize;
                for x in 0..tw {
                    let s = src_row + (x * 2 * 4) as usize;
                    let d = dst_row + (x * 4) as usize;
                    scaled[d..d + 4].copy_from_slice(&bgra[s..s + 4]);
                }
            }
            (tw, th, scaled)
        } else {
            (width, height, bgra.to_vec())
        };

        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&src_data, 1);
        let mut payload = Vec::with_capacity(4 + compressed.len());
        payload.extend_from_slice(&(target_w as u16).to_le_bytes());
        payload.extend_from_slice(&(target_h as u16).to_le_bytes());
        payload.extend_from_slice(&compressed);

        Ok(EncodedVideoFrame {
            frame_id,
            is_keyframe,
            timestamp_us,
            data: payload,
        })
    }

    pub fn config(&self) -> &VideoEncoderConfig {
        &self.config
    }
}
