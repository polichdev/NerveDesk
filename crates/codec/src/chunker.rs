use std::collections::HashMap;
use std::time::Instant;
use tracing::trace;

use common::{NerveError, Result, VideoChunkHeader, MAX_VIDEO_PAYLOAD_SIZE};

pub const FEC_GROUP_SIZE: u16 = 8;

pub struct VideoFrameChunker;

impl VideoFrameChunker {
    pub fn chunk_frame(
        frame_id: u64,
        is_keyframe: bool,
        timestamp_us: u64,
        frame_data: &[u8],
    ) -> Vec<Vec<u8>> {
        Self::chunk_frame_with_fec(frame_id, is_keyframe, timestamp_us, frame_data, true)
    }

    pub fn chunk_frame_with_fec(
        frame_id: u64,
        is_keyframe: bool,
        timestamp_us: u64,
        frame_data: &[u8],
        enable_fec: bool,
    ) -> Vec<Vec<u8>> {
        let total_size = frame_data.len();
        if total_size == 0 {
            return Vec::new();
        }

        let total_chunks =
            ((total_size + MAX_VIDEO_PAYLOAD_SIZE - 1) / MAX_VIDEO_PAYLOAD_SIZE) as u16;
        let fec_chunk_count = if enable_fec && total_chunks > 1 {
            (total_chunks + FEC_GROUP_SIZE - 1) / FEC_GROUP_SIZE
        } else {
            0
        };
        let mut packets = Vec::with_capacity((total_chunks + fec_chunk_count) as usize);

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

        if enable_fec && total_chunks > 1 {
            for group_idx in 0..fec_chunk_count {
                let start_chunk = group_idx * FEC_GROUP_SIZE;
                let chunk_count = FEC_GROUP_SIZE.min(total_chunks - start_chunk);
                if chunk_count <= 1 {
                    continue;
                }

                let mut max_len = 0;
                for idx in start_chunk..start_chunk + chunk_count {
                    let c_start = (idx as usize) * MAX_VIDEO_PAYLOAD_SIZE;
                    let c_end = (c_start + MAX_VIDEO_PAYLOAD_SIZE).min(total_size);
                    max_len = max_len.max(c_end - c_start);
                }

                let mut parity = vec![0u8; max_len];
                for idx in start_chunk..start_chunk + chunk_count {
                    let c_start = (idx as usize) * MAX_VIDEO_PAYLOAD_SIZE;
                    let c_end = (c_start + MAX_VIDEO_PAYLOAD_SIZE).min(total_size);
                    let payload = &frame_data[c_start..c_end];
                    for (p, &b) in parity.iter_mut().zip(payload.iter()) {
                        *p ^= b;
                    }
                }

                let fec_header = VideoChunkHeader::new_fec(
                    frame_id,
                    group_idx,
                    total_chunks,
                    parity.len() as u16,
                    start_chunk,
                    chunk_count,
                    timestamp_us,
                );

                let mut fec_packet = vec![0u8; header_size + parity.len()];
                fec_packet[..header_size].copy_from_slice(bytemuck::bytes_of(&fec_header));
                fec_packet[header_size..].copy_from_slice(&parity);
                packets.push(fec_packet);
            }
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
    fec_groups: HashMap<u16, (u16, u16, Vec<u8>)>,
}

impl IncompleteFrame {
    fn try_recover(&mut self) -> bool {
        let mut recovered_any = false;
        for (&_grp_idx, &(start_chunk, count, ref parity)) in &self.fec_groups {
            let end_chunk = (start_chunk + count) as usize;
            let mut missing_idx = None;
            let mut missing_count = 0;
            for idx in (start_chunk as usize)..end_chunk {
                if idx < self.chunks.len() && self.chunks[idx].is_none() {
                    missing_idx = Some(idx);
                    missing_count += 1;
                    if missing_count > 1 {
                        break;
                    }
                }
            }

            if missing_count == 1 {
                let missing = missing_idx.unwrap();
                let mut recovered = parity.clone();
                for idx in (start_chunk as usize)..end_chunk {
                    if idx != missing {
                        if let Some(ref c) = self.chunks[idx] {
                            for (r, &b) in recovered.iter_mut().zip(c.iter()) {
                                *r ^= b;
                            }
                        }
                    }
                }
                self.chunks[missing] = Some(recovered);
                self.received_count += 1;
                recovered_any = true;
            }
        }
        recovered_any
    }
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
            return Err(NerveError::Protocol(
                "Video packet smaller than header".into(),
            ));
        }

        let header: VideoChunkHeader = *bytemuck::from_bytes(&packet_bytes[..header_size]);
        header.validate()?;

        if header.frame_id <= self.last_completed_frame_id {
            trace!("Discarding stale video chunk for frame {}", header.frame_id);
            return Ok(None);
        }

        let payload = &packet_bytes[header_size..];
        if payload.len() != header.payload_size as usize {
            return Err(NerveError::Protocol(
                "Video chunk payload size mismatch".into(),
            ));
        }

        self.cleanup_stale_frames();

        if !self.frames.contains_key(&header.frame_id) && self.frames.len() >= 4 {
            if let Some(oldest_id) = self.frames.keys().min().copied() {
                self.frames.remove(&oldest_id);
            }
        }

        let total_chunks = header.total_chunks as usize;
        let entry = self
            .frames
            .entry(header.frame_id)
            .or_insert_with(|| IncompleteFrame {
                chunks: vec![None; total_chunks],
                received_count: 0,
                total_chunks: header.total_chunks,
                is_keyframe: header.is_keyframe(),
                timestamp_us: header.timestamp_us,
                created_at: Instant::now(),
                fec_groups: HashMap::new(),
            });

        if header.is_fec_parity() {
            let (start_chunk, count) = header.fec_group_range();
            entry.fec_groups.insert(header.chunk_index, (start_chunk, count, payload.to_vec()));
            entry.try_recover();
        } else {
            let chunk_idx = header.chunk_index as usize;
            if chunk_idx < entry.chunks.len() && entry.chunks[chunk_idx].is_none() {
                entry.chunks[chunk_idx] = Some(payload.to_vec());
                entry.received_count += 1;
                entry.try_recover();
            }
        }

        if entry.received_count == entry.total_chunks {
            let incomplete = self.frames.remove(&header.frame_id).unwrap();
            let mut full_frame = Vec::new();
            for chunk in incomplete.chunks {
                if let Some(c) = chunk {
                    full_frame.extend_from_slice(&c);
                } else {
                    return Err(NerveError::Protocol(
                        "Missing chunk in completed frame".into(),
                    ));
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
        self.frames
            .retain(|_, frame| now.duration_since(frame.created_at).as_millis() <= max_age);
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

        let packets =
            VideoFrameChunker::chunk_frame(frame_id, is_keyframe, timestamp_us, &original_data);

        assert_eq!(packets.len(), 5); // 4 data chunks + 1 FEC parity chunk

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

    #[test]
    fn test_fec_packet_loss_recovery() {
        let mut original_data = vec![0u8; 5000];
        for (i, b) in original_data.iter_mut().enumerate() {
            *b = ((i * 7 + 13) % 256) as u8;
        }

        let frame_id = 99;
        let is_keyframe = false;
        let timestamp_us = 999999;

        // Chunk with FEC enabled (produces 4 data chunks + 1 FEC parity chunk)
        let packets = VideoFrameChunker::chunk_frame_with_fec(
            frame_id,
            is_keyframe,
            timestamp_us,
            &original_data,
            true,
        );
        assert_eq!(packets.len(), 5);

        // Simulate network dropping chunk index 2 (the 3rd data chunk)
        let mut lossy_packets = Vec::new();
        for (i, p) in packets.into_iter().enumerate() {
            if i == 2 {
                continue; // Drop packet 2
            }
            lossy_packets.push(p);
        }
        assert_eq!(lossy_packets.len(), 4);

        let mut reassembler = VideoFrameReassembler::new();
        let mut completed = None;

        for packet in lossy_packets {
            if let Some(res) = reassembler.feed_packet(&packet).unwrap() {
                completed = Some(res);
            }
        }

        // Must still completely recover the frame despite the dropped packet!
        let (out_id, out_kf, out_ts, out_data) = completed.expect("FEC should recover lost packet");
        assert_eq!(out_id, frame_id);
        assert_eq!(out_kf, is_keyframe);
        assert_eq!(out_ts, timestamp_us);
        assert_eq!(out_data, original_data);
    }
}
