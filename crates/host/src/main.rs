use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, warn};

use capture::{DxgiCaptureConfig, DxgiOutputDuplicator};
use codec::{HardwareVideoEncoder, VideoEncoderConfig};
use common::InputEvent;
use input::InputHookManager;
use net::{UdpInputSender, VideoSender};

#[derive(Parser, Debug)]
#[command(name = "host")]
#[command(about = "NerveDesk Host Daemon - Low-latency input and display streaming")]
struct Args {
    #[arg(long, env = "NERVEDESK_PAIRING_KEY")]
    security_key: String,
    #[arg(short, long, default_value = "127.0.0.1:48100")]
    target: SocketAddr,

    #[arg(long, default_value = "127.0.0.1:48101")]
    video_target: SocketAddr,

    #[arg(long, default_value_t = false)]
    trap: bool,

    #[arg(long, default_value_t = false)]
    stream_video: bool,

    #[arg(long, default_value_t = 15)]
    bitrate_mbps: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,host=debug,input=debug,net=debug,capture=debug,codec=debug".into()
            }),
        )
        .init();

    let args = Args::parse();
    info!(
        "Starting NerveDesk Host Daemon v{}",
        env!("CARGO_PKG_VERSION")
    );

    let input_sender = UdpInputSender::new(args.target, args.security_key.clone())
        .await
        .context("Failed to initialize input UDP sender")?;

    let (event_tx, event_rx) = crossbeam_channel::unbounded::<InputEvent>();

    let hook_manager =
        InputHookManager::start(event_tx).context("Failed to install low-level Windows hooks")?;

    if args.trap {
        hook_manager.set_capturing(true);
    }

    let video_worker = if args.stream_video {
        let video_target = args.video_target;
        let bitrate_bps = args.bitrate_mbps * 1_000_000;

        Some(tokio::spawn(async move {
            let video_sender = match VideoSender::new(video_target, args.security_key).await {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to initialize VideoSender: {:?}", e);
                    return;
                }
            };

            let config = DxgiCaptureConfig::default();
            let mut duplicator = match DxgiOutputDuplicator::new(config) {
                Ok(dup) => dup,
                Err(e) => {
                    error!("Failed to initialize DXGI duplicator: {:?}", e);
                    return;
                }
            };

            let (w, h) = duplicator.dimensions();
            let encoder_config = VideoEncoderConfig {
                width: w,
                height: h,
                fps: 60,
                bitrate_bps,
                keyframe_interval: 120,
            };

            let encoder = match HardwareVideoEncoder::new(encoder_config) {
                Ok(enc) => enc,
                Err(e) => {
                    error!("Failed to initialize HardwareVideoEncoder: {:?}", e);
                    return;
                }
            };

            let mut frames_streamed = 0u64;
            let mut bytes_streamed = 0usize;
            let mut last_report = Instant::now();

            loop {
                let now_us = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;

                match duplicator.acquire_frame() {
                    Ok(Some(frame)) => match encoder.encode_frame(&frame.texture, now_us, false) {
                        Ok(encoded_frame) => match video_sender.send_frame(&encoded_frame).await {
                            Ok(sent_bytes) => {
                                frames_streamed += 1;
                                bytes_streamed += sent_bytes;
                            }
                            Err(e) => {
                                warn!("Failed to stream video frame: {:?}", e);
                            }
                        },
                        Err(e) => {
                            warn!("Hardware encoding error: {:?}", e);
                        }
                    },
                    Ok(None) => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    Err(e) => {
                        warn!("Capture error: {:?}", e);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }

                if last_report.elapsed() >= Duration::from_secs(2) {
                    if frames_streamed > 0 {
                        let elapsed_sec = last_report.elapsed().as_secs_f64();
                        let fps = frames_streamed as f64 / elapsed_sec;
                        let mbps = (bytes_streamed as f64 * 8.0) / (elapsed_sec * 1_000_000.0);
                        info!(
                            "Video Streaming: {:.1} FPS | {:.2} Mbps | Target: {}",
                            fps, mbps, video_target
                        );
                        frames_streamed = 0;
                        bytes_streamed = 0;
                    }
                    last_report = Instant::now();
                }
            }
        }))
    } else {
        None
    };

    let input_worker = tokio::spawn(async move {
        let mut packet_count = 0u64;
        let mut last_report = tokio::time::Instant::now();

        loop {
            match event_rx.try_recv() {
                Ok(event) => {
                    if let Err(e) = input_sender.send_event(&event).await {
                        warn!("Failed to emit input UDP packet: {:?}", e);
                    } else {
                        packet_count += 1;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_micros(500)).await;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    break;
                }
            }

            if last_report.elapsed() >= Duration::from_secs(5) {
                if packet_count > 0 {
                    info!("Input stats: {} packets streamed in last 5s", packet_count);
                    packet_count = 0;
                }
                last_report = tokio::time::Instant::now();
            }
        }
    });

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            info!("Received shutdown signal (Ctrl+C). Cleaning up...");
        }
        res = input_worker => {
            if let Err(e) = res {
                error!("Input worker error: {:?}", e);
            }
        }
    }

    drop(hook_manager);
    if let Some(worker) = video_worker {
        worker.abort();
    }

    Ok(())
}
