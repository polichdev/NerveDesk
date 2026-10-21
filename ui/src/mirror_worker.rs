use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use capture::{DxgiCaptureConfig, DxgiOutputDuplicator};
use tracing::{error, warn};

use crate::mirror_stream::{encode_bgra, MirrorSender};
use crate::window_capture::{CaptureApartment, WindowCapture};

enum Source {
    Screen { duplicator: DxgiOutputDuplicator, pixels: Vec<u8> },
    Window(WindowCapture),
}

impl Source {
    fn new(window_id: Option<String>, screen_config: Option<DxgiCaptureConfig>) -> Result<Self, String> {
        if let Some(id) = window_id {
            return WindowCapture::new(&id).map(Self::Window);
        }
        let config = screen_config.ok_or("Missing screen capture configuration")?;
        let dup = DxgiOutputDuplicator::new(config).map_err(|e| e.to_string())?;
        let (w, h) = dup.dimensions();
        Ok(Self::Screen { duplicator: dup, pixels: vec![0u8; w as usize * h as usize * 4] })
    }

    fn next_jpeg(&mut self) -> Result<Option<(u32, u32, i16, i16, Vec<u8>)>, String> {
        match self {
            Self::Screen { duplicator, pixels } => {
                let Some(frame) = duplicator.acquire_frame().map_err(|e| e.to_string())? else {
                    return Ok(None);
                };
                let len = frame.width as usize * frame.height as usize * 4;
                pixels.resize(len, 0);
                frame.copy_to_cpu(pixels).map_err(|e| e.to_string())?;
                let jpeg = encode_bgra(frame.width, frame.height, pixels)?;
                Ok(Some((frame.width, frame.height, 0, 0, jpeg)))
            }
            Self::Window(cap) => {
                let Some((w, h, pix)) = cap.next_frame()? else { return Ok(None); };
                let (ox, oy) = cap.window_offset();
                let jpeg = encode_bgra(w, h, &pix)?;
                Ok(Some((w, h, ox, oy, jpeg)))
            }
        }
    }
}

pub async fn start(
    target: SocketAddr,
    window_id: Option<String>,
    screen_config: Option<DxgiCaptureConfig>,
    cancel: Arc<AtomicBool>,
    projecting: Arc<AtomicBool>,
    fps: Arc<AtomicU64>,
    mbps: Arc<AtomicU64>,
    generation: Arc<AtomicU64>,
    current_generation: u64,
) -> Result<tokio::task::JoinHandle<()>, String> {
    let mut sender = MirrorSender::connect(target).await?;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

    let worker = tokio::task::spawn_blocking(move || {
        let _apartment = if window_id.is_some() {
            match CaptureApartment::initialize() {
                Ok(a) => Some(a),
                Err(e) => { let _ = ready_tx.send(Err(e)); return; }
            }
        } else { None };

        let mut source = match Source::new(window_id, screen_config) {
            Ok(s) => { let _ = ready_tx.send(Ok(())); s }
            Err(e) => { let _ = ready_tx.send(Err(e)); return; }
        };

        let emulator = input::InputEmulator::new();

        let mut receiver_ready = true;
        let mut last_frame: Option<(u32, u32, i16, i16, Vec<u8>)> = None;
        let mut last_sent = Instant::now();
        let mut stat_frames = 0u64;
        let mut stat_bytes  = 0u64;
        let mut stat_report = Instant::now();

        while !cancel.load(Ordering::Relaxed)
            && projecting.load(Ordering::Relaxed)
            && generation.load(Ordering::SeqCst) == current_generation
        {
            let cycle = Instant::now();

            match sender.read_inbound_messages(&emulator) {
                Ok(got_ack) => {
                    if got_ack { receiver_ready = true; }
                }
                Err(e) => {
                    warn!("Mirror stream broken: {e}");
                    break;
                }
            }

            if !receiver_ready && last_sent.elapsed() >= Duration::from_millis(100) {
                receiver_ready = true;
            }

            match source.next_jpeg() {
                Ok(Some(f)) => { last_frame = Some(f); }
                Ok(None)    => {}
                Err(e)      => { warn!("Mirror capture error: {e}"); break; }
            }

            if receiver_ready {
                if let Some((w, h, ox, oy, ref jpeg)) = last_frame {
                    match sender.send(w, h, ox, oy, jpeg) {
                        Ok(n) => {
                            stat_bytes  += n as u64;
                            stat_frames += 1;
                            last_sent       = Instant::now();
                            receiver_ready  = false;
                        }
                        Err(e) => { error!("Mirror send failed: {e}"); break; }
                    }
                }
            }

            if stat_report.elapsed() >= Duration::from_secs(1) {
                let secs = stat_report.elapsed().as_secs_f64();
                fps .store((stat_frames as f64 / secs * 10.0) as u64, Ordering::Relaxed);
                mbps.store((stat_bytes as f64 * 8.0 / secs / 100_000.0) as u64, Ordering::Relaxed);
                stat_frames = 0;
                stat_bytes  = 0;
                stat_report = Instant::now();
            }

            let elapsed = cycle.elapsed();
            let target_cycle = Duration::from_millis(8);
            if elapsed < target_cycle {
                std::thread::sleep(target_cycle - elapsed);
            }
        }

        if generation.load(Ordering::SeqCst) == current_generation {
            projecting.store(false, Ordering::SeqCst);
            fps .store(0, Ordering::Relaxed);
            mbps.store(0, Ordering::Relaxed);
        }
    });

    ready_rx.await.map_err(|_| "Mirror worker failed to start".to_string())??;
    Ok(worker)
}
