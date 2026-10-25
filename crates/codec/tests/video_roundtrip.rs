use codec::{
    HardwareVideoDecoder, HardwareVideoEncoder, VideoDecoderConfig, VideoEncoderConfig,
    VideoFrameChunker, VideoFrameReassembler,
};

#[test]
fn bgra_frame_survives_transport_packetization() {
    let width = 64;
    let height = 32;
    let pixels: Vec<u8> = (0..width * height * 4)
        .map(|index| ((index * 37) % 251) as u8)
        .collect();
    let encoder = HardwareVideoEncoder::new(VideoEncoderConfig::default()).unwrap();
    let frame = encoder
        .encode_bgra(width, height, &pixels, 123_456, true)
        .unwrap();
    let packets = VideoFrameChunker::chunk_frame(
        frame.frame_id,
        frame.is_keyframe,
        frame.timestamp_us,
        &frame.data,
    );
    let mut reassembler = VideoFrameReassembler::new();
    let mut completed = None;
    for packet in packets {
        if let Some(frame) = reassembler.feed_packet(&packet).unwrap() {
            completed = Some(frame);
        }
    }
    let (id, _, timestamp, payload) = completed.expect("frame should reassemble");
    let mut decoder = HardwareVideoDecoder::new(VideoDecoderConfig::default()).unwrap();
    let decoded = decoder.decode_frame(id, timestamp, &payload).unwrap();
    assert_eq!((decoded.width, decoded.height), (width, height));
    assert_eq!(decoded.bgra, pixels);
}

#[test]
fn av1_roundtrip_encodes_and_decodes_frames() {
    let width = 64;
    let height = 32;
    let pixels: Vec<u8> = (0..width * height * 4)
        .map(|index| ((index * 37) % 251) as u8)
        .collect();
    let Ok(mut encoder) = codec::av1::Encoder::new(width, height) else {
        return;
    };
    let Ok(mut decoder) = codec::av1::Decoder::new(width, height) else {
        return;
    };

    let frame1 = encoder.encode(&pixels).unwrap();
    assert!(!frame1.is_empty());
    let decoded1 = decoder.decode(&frame1).unwrap();
    assert!(decoded1.is_some());
    let bgra1 = decoded1.unwrap();
    assert_eq!(bgra1.len(), (width * height * 4) as usize);

    let frame2 = encoder.encode(&pixels).unwrap();
    assert!(!frame2.is_empty());
    let decoded2 = decoder.decode(&frame2).unwrap();
    assert!(decoded2.is_some());
    let bgra2 = decoded2.unwrap();
    assert_eq!(bgra2.len(), (width * height * 4) as usize);
}
