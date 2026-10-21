use std::collections::HashMap;
use std::time::Instant;
use tracing::trace;

use common::{
    NerveError, Result, VideoChunkHeader, MAX_VIDEO_PAYLOAD_SIZE,
};

pub struct VideoFrameChunker;

impl VideoFrameChunker {
    pub fn chunk_frame(
        frame_id: u64,
        is_keyframe: bool,
        timestamp_us: u64,
        frame_data: &[u8],
    ) -> Vec<Vec<u8>> {
        let total_size = frame_data.len();
        if total_size == 0 {
            return Vec::new();
        }

        let total_chunks = ((total_size + MAX_VIDEO_PAYLOAD_SIZE - 1)
            / MAX_VIDEO_PAYLOAD_SIZE) as u16;
        let mut packets = Vec::with_capacity(total_chunks as usize);

        let header_size = std::mem::size_of::<VideoChunkHeader>();

        for chunk_idx in 0..total_chunks {
            let start = (chunk_idx as usize) * MAX_VIDEO_PAYLOAD_SIZE;
            let end = (start + MAX_VIDEO_PAYLOAD_SIZE).min(total_size);
            let payload = &frame_data[start..end];
            let is_last = chunk_idx + 1 == total_chunks;

            let header = VideoChunkHeader::new(
                frame_id,
                chunk_idx,
                total_chunks,
                payload.len() as u16,
                is_keyframe,
                is_last,
                timestamp_us,
            );

            let mut packet = vec![0u8; header_size + payload.len()];
            packet[..header_size].copy_from_slice(bytemuck::bytes_of(&header));
            packet[header_size..].copy_from_slice(payload);

            packets.push(packet);
        }

        packets
    }
}

struct IncompleteFrame {
    chunks: Vec<Option<Vec<u8>>>,
    received_count: u16,
    total_chunks: u16,
    is_keyframe: bool,
    timestamp_us: u64,
    created_at: Instant,
}

pub struct VideoFrameReassembler {
    frames: HashMap<u64, IncompleteFrame>,
    last_completed_frame_id: u64,
    max_frame_age_ms: u128,
}

impl VideoFrameReassembler {
    pub fn new() -> Self {
        Self {
            frames: HashMap::new(),
            last_completed_frame_id: 0,
            max_frame_age_ms: 500,
        }
    }

    pub fn feed_packet(
        &mut self,
        packet_bytes: &[u8],
    ) -> Result<Option<(u64, bool, u64, Vec<u8>)>> {
        let header_size = std::mem::size_of::<VideoChunkHeader>();
        if packet_bytes.len() < header_size {
            return Err(NerveError::Protocol("Video packet smaller than header".into()));
        }

        let header: VideoChunkHeader = *bytemuck::from_bytes(&packet_bytes[..header_size]);
        header.validate()?;

        if header.frame_id <= self.last_completed_frame_id {
            trace!("Discarding stale video chunk for frame {}", header.frame_id);
            return Ok(None);
        }

        let payload = &packet_bytes[header_size..];
        if payload.len() != header.payload_size as usize {
            return Err(NerveError::Protocol("Video chunk payload size mismatch".into()));
        }

        self.cleanup_stale_frames();

        if !self.frames.contains_key(&header.frame_id) && self.frames.len() >= 4 {
            if let Some(oldest_id) = self.frames.keys().min().copied() {
                self.frames.remove(&oldest_id);
            }
        }

        let total_chunks = header.total_chunks as usize;
        let entry = self.frames.entry(header.frame_id).or_insert_with(|| {
            IncompleteFrame {
                chunks: vec![None; total_chunks],
                received_count: 0,
                total_chunks: header.total_chunks,
                is_keyframe: header.is_keyframe(),
                timestamp_us: header.timestamp_us,
                created_at: Instant::now(),
            }
        });

        let chunk_idx = header.chunk_index as usize;
        if chunk_idx < entry.chunks.len() && entry.chunks[chunk_idx].is_none() {
            entry.chunks[chunk_idx] = Some(payload.to_vec());
            entry.received_count += 1;
        }

        if entry.received_count == entry.total_chunks {
            let incomplete = self.frames.remove(&header.frame_id).unwrap();
            let mut full_frame = Vec::new();
            for chunk in incomplete.chunks {
                if let Some(c) = chunk {
                    full_frame.extend_from_slice(&c);
                } else {
                    return Err(NerveError::Protocol("Missing chunk in completed frame".into()));
                }
            }

            self.last_completed_frame_id = header.frame_id;
            return Ok(Some((
                header.frame_id,
                incomplete.is_keyframe,
                incomplete.timestamp_us,
                full_frame,
            )));
        }

        Ok(None)
    }

    fn cleanup_stale_frames(&mut self) {
        let now = Instant::now();
        let max_age = self.max_frame_age_ms;
        self.frames.retain(|_, frame| {
            now.duration_since(frame.created_at).as_millis() <= max_age
        });
    }
}

impl Default for VideoFrameReassembler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_and_reassemble() {
        let mut original_data = vec![0u8; 5000];
        for (i, b) in original_data.iter_mut().enumerate() {
            *b = (i % 256) as u8;
        }

        let frame_id = 42;
        let is_keyframe = true;
        let timestamp_us = 12345678;

        let packets = VideoFrameChunker::chunk_frame(
            frame_id,
            is_keyframe,
            timestamp_us,
            &original_data,
        );

        assert_eq!(packets.len(), 4);

        let mut reassembler = VideoFrameReassembler::new();
        let mut completed = None;

        for packet in packets {
            if let Some(res) = reassembler.feed_packet(&packet).unwrap() {
                completed = Some(res);
            }
        }

        let (out_id, out_kf, out_ts, out_data) = completed.expect("Should reassemble");
        assert_eq!(out_id, frame_id);
        assert_eq!(out_kf, is_keyframe);
        assert_eq!(out_ts, timestamp_us);
        assert_eq!(out_data, original_data);
    }
}
