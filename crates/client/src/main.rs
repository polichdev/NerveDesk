use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, trace, warn};

use codec::{HardwareVideoDecoder, VideoDecoderConfig};
use input::InputEmulator;
use net::{UdpInputReceiver, VideoReceiver};

#[derive(Parser, Debug)]
#[command(name = "client")]
#[command(about = "NerveDesk Client - Low-latency input receiver and video player")]
struct Args {
    #[arg(short, long, default_value = "0.0.0.0:48100")]
    bind: SocketAddr,

    #[arg(long, default_value = "0.0.0.0:48101")]
    video_bind: SocketAddr,

    #[arg(long, default_value_t = false)]
    recv_video: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,client=debug,net=debug,codec=debug".into()
            }),
        )
        .init();

    let args = Args::parse();
    info!("Starting NerveDesk Client Service v{}", env!("CARGO_PKG_VERSION"));

    let mut input_receiver = UdpInputReceiver::bind(args.bind)
        .await
        .context("Failed to bind UDP input receiver")?;

    let emulator = InputEmulator::new();

    let video_worker = if args.recv_video {
        let video_bind = args.video_bind;
        Some(tokio::spawn(async move {
            let mut video_receiver = match VideoReceiver::bind(video_bind).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Failed to bind VideoReceiver: {:?}", e);
                    return;
                }
            };

            let decoder_config = VideoDecoderConfig::default();
            let mut decoder = match HardwareVideoDecoder::new(decoder_config) {
                Ok(d) => d,
                Err(e) => {
                    error!("Failed to initialize HardwareVideoDecoder: {:?}", e);
                    return;
                }
            };

            let mut frames_received = 0u64;
            let mut total_latency_us = 0u64;
            let mut last_report = Instant::now();

            loop {
                match video_receiver.recv_frame().await {
                    Ok((frame_id, is_keyframe, timestamp_us, frame_data)) => {
                        let now_us = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_micros() as u64;

                        let latency_us = now_us.saturating_sub(timestamp_us);
                        total_latency_us += latency_us;
                        frames_received += 1;

                        if let Err(e) = decoder.decode_frame(frame_id, timestamp_us, &frame_data) {
                            warn!("Failed to decode video frame {}: {:?}", frame_id, e);
                        }

                        if last_report.elapsed() >= Duration::from_secs(2) {
                            if frames_received > 0 {
                                let fps = frames_received as f64 / last_report.elapsed().as_secs_f64();
                                let avg_latency_ms =
                                    (total_latency_us as f64 / frames_received as f64) / 1000.0;
                                info!(
                                    "Video Playback: {:.1} FPS | Avg Latency: {:.2} ms | Keyframe: {}",
                                    fps, avg_latency_ms, is_keyframe
                                );
                                frames_received = 0;
                                total_latency_us = 0;
                            }
                            last_report = Instant::now();
                        }
                    }
                    Err(e) => {
                        warn!("Video receive error: {:?}", e);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }))
    } else {
        None
    };

    let mut packet_count = 0u64;
    let mut total_latency_us = 0u64;
    let mut last_report = Instant::now();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Received shutdown signal (Ctrl+C). Terminating client...");
                break;
            }
            res = input_receiver.recv_event() => {
                match res {
                    Ok((header, event, peer)) => {
                        let now_us = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_micros() as u64;

                        let latency_us = now_us.saturating_sub(header.timestamp_us);
                        total_latency_us += latency_us;
                        packet_count += 1;

                        trace!(
                            "Received event from {}: seq={} latency={:.2}ms event={:?}",
                            peer,
                            header.sequence,
                            latency_us as f64 / 1000.0,
                            event
                        );

                        if let Err(e) = emulator.inject(&event) {
                            warn!("Failed to inject input event: {:?}", e);
                        }

                        if last_report.elapsed() >= Duration::from_secs(5) {
                            if packet_count > 0 {
                                let avg_latency_ms = (total_latency_us as f64 / packet_count as f64) / 1000.0;
                                info!(
                                    "Diagnostics: {} packets processed in last 5s | Avg Latency: {:.3} ms",
                                    packet_count,
                                    avg_latency_ms
                                );
                                packet_count = 0;
                                total_latency_us = 0;
                            }
                            last_report = Instant::now();
                        }
                    }
                    Err(e) => {
                        error!("Error receiving UDP input packet: {:?}", e);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }

    if let Some(worker) = video_worker {
        worker.abort();
    }

    Ok(())
}
