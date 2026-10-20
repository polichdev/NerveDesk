use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tauri::{command, State};
use tracing::{error, info};

use capture::{DxgiCaptureConfig, DxgiOutputDuplicator, VirtualDisplayManager};
use codec::{HardwareVideoDecoder, HardwareVideoEncoder, VideoDecoderConfig, VideoEncoderConfig};
use common::InputEvent;
use input::{InputEmulator, InputHookManager};
use net::{
    DeviceRole, DiscoveryBeacon, DiscoveryBeaconEmitter, DiscoveryListener,
    UdpInputReceiver, UdpInputSender, VideoReceiver, VideoSender,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMetrics {
    pub mode: String,
    pub is_active: bool,
    pub rtt_ms: f64,
    pub video_fps: f64,
    pub bandwidth_mbps: f64,
    pub peer_device: Option<String>,
    pub virtual_display_active: bool,
    pub screen_edge: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub name: String,
    pub role: String,
    pub ip: String,
    pub input_port: u16,
    pub video_port: u16,
    pub screen_width: u32,
    pub screen_height: u32,
}

pub struct AppState {
    pub is_running: Arc<AtomicBool>,
    pub mode: Mutex<String>,
    pub peer_device: Mutex<Option<String>>,
    pub screen_edge: Mutex<String>,
    pub display_manager: VirtualDisplayManager,
    pub hook_manager: Mutex<Option<InputHookManager>>,
    pub active_workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    pub live_rtt_us: Arc<AtomicU64>,
    pub live_fps_x10: Arc<AtomicU64>,
    pub live_mbps_x10: Arc<AtomicU64>,
    pub discovered_devices: Arc<Mutex<HashMap<String, DiscoveredDevice>>>,
}

fn stop_service_internal(state: &AppState) {
    state.is_running.store(false, Ordering::SeqCst);
    if let Ok(mut guard) = state.hook_manager.lock() {
        if let Some(mut hook) = guard.take() {
            hook.stop();
        }
    }
    if let Ok(mut workers) = state.active_workers.lock() {
        for w in workers.drain(..) {
            w.abort();
        }
    }
    let _ = state.display_manager.unplug_display();
    *state.mode.lock().unwrap() = "Idle".into();
    *state.peer_device.lock().unwrap() = None;
    state.live_rtt_us.store(0, Ordering::Relaxed);
    state.live_fps_x10.store(0, Ordering::Relaxed);
    state.live_mbps_x10.store(0, Ordering::Relaxed);
}

#[command]
fn get_my_ip() -> String {
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                return addr.ip().to_string();
            }
        }
    }
    "127.0.0.1".into()
}

#[command]
fn get_metrics(state: State<AppState>) -> AppMetrics {
    let mode = state.mode.lock().unwrap().clone();
    let peer = state.peer_device.lock().unwrap().clone();
    let edge = state.screen_edge.lock().unwrap().clone();
    let is_active = state.is_running.load(Ordering::SeqCst);
    let v_display = state.display_manager.is_plugged();

    let rtt_us = state.live_rtt_us.load(Ordering::Relaxed);
    let rtt_ms = if is_active { (rtt_us as f64) / 1000.0 } else { 0.0 };
    let fps = if is_active { (state.live_fps_x10.load(Ordering::Relaxed) as f64) / 10.0 } else { 0.0 };
    let mbps = if is_active { (state.live_mbps_x10.load(Ordering::Relaxed) as f64) / 10.0 } else { 0.0 };

    AppMetrics {
        mode,
        is_active,
        rtt_ms,
        video_fps: fps,
        bandwidth_mbps: mbps,
        peer_device: peer,
        virtual_display_active: v_display,
        screen_edge: edge,
    }
}

#[command]
async fn discover_devices(state: State<'_, AppState>) -> Result<Vec<DiscoveredDevice>, String> {
    let my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Host-PC".into());
    let sw = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN) } as u32;
    let sh = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN) } as u32;
    let beacon = DiscoveryBeacon::new(my_name, DeviceRole::Host, 48100, 48101, sw, sh);

    if let Ok(emitter) = DiscoveryBeaconEmitter::new(beacon).await {
        let _ = emitter.emit_beacon().await;
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    let devices: Vec<DiscoveredDevice> = if let Ok(guard) = state.discovered_devices.lock() {
        guard.values().cloned().collect()
    } else {
        Vec::new()
    };

    Ok(devices)
}

#[command]
async fn start_host(
    target_ip: String,
    stream_video: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    stop_service_internal(&state);

    let target_addr: SocketAddr = if target_ip.contains(':') {
        target_ip.parse().map_err(|e| format!("Invalid target address: {e}"))?
    } else {
        format!("{target_ip}:48100")
            .parse()
            .map_err(|e| format!("Invalid target address: {e}"))?
    };

    let video_target: SocketAddr = SocketAddr::new(target_addr.ip(), 48101);

    info!("Starting internal Host engine -> input={}, video={}", target_addr, video_target);

    let input_sender = UdpInputSender::new(target_addr)
        .await
        .map_err(|e| format!("Failed to create UDP input sender: {e}"))?;

    let (event_tx, event_rx) = crossbeam_channel::unbounded::<InputEvent>();
    let hook_manager = InputHookManager::start(event_tx)
        .map_err(|e| format!("Failed to install input hooks: {e}"))?;

    let edge_str = state.screen_edge.lock().unwrap().clone();
    let screen_edge = match edge_str.as_str() {
        "Left" => input::ScreenEdge::Left,
        "Right" => input::ScreenEdge::Right,
        "Top" => input::ScreenEdge::Top,
        "Bottom" => input::ScreenEdge::Bottom,
        _ => input::ScreenEdge::Right,
    };

    let (mut client_w, mut client_h) = (0, 0);
    if let Ok(guard) = state.discovered_devices.lock() {
        if let Some(dev) = guard.get(&target_addr.ip().to_string()) {
            client_w = dev.screen_width as i32;
            client_h = dev.screen_height as i32;
        }
    }
    input::set_active_edge_and_resolution(screen_edge, client_w, client_h);

    let is_running_in = state.is_running.clone();

    let input_worker = tokio::spawn(async move {
        while is_running_in.load(Ordering::SeqCst) {
            match event_rx.try_recv() {
                Ok(event) => {
                    let _ = input_sender.send_event(&event).await;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_micros(500)).await;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }
    });

    let mut workers = vec![input_worker];

    if stream_video {
        let _ = state.display_manager.plug_display();
        let is_running_vid = state.is_running.clone();
        let fps_holder = state.live_fps_x10.clone();
        let mbps_holder = state.live_mbps_x10.clone();

        let vid_worker = tokio::spawn(async move {
            let video_sender = match VideoSender::new(video_target).await {
                Ok(s) => s,
                Err(e) => {
                    error!("VideoSender error: {:?}", e);
                    return;
                }
            };

            let config = DxgiCaptureConfig::default();
            let mut duplicator = match DxgiOutputDuplicator::new(config) {
                Ok(d) => d,
                Err(e) => {
                    error!("DxgiDuplicator error: {:?}", e);
                    return;
                }
            };

            let (w, h) = duplicator.dimensions();
            let encoder_config = VideoEncoderConfig {
                width: w,
                height: h,
                fps: 60,
                bitrate_bps: 15_000_000,
                keyframe_interval: 120,
            };

            let encoder = match HardwareVideoEncoder::new(encoder_config) {
                Ok(enc) => enc,
                Err(e) => {
                    error!("HardwareVideoEncoder error: {:?}", e);
                    return;
                }
            };

            let mut frames_streamed = 0u64;
            let mut bytes_streamed = 0usize;
            let mut last_report = Instant::now();

            while is_running_vid.load(Ordering::SeqCst) {
                let now_us = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;

                match duplicator.acquire_frame() {
                    Ok(Some(frame)) => {
                        if let Ok(encoded) = encoder.encode_frame(&frame.texture, now_us, false) {
                            if let Ok(sent) = video_sender.send_frame(&encoded).await {
                                frames_streamed += 1;
                                bytes_streamed += sent;
                            }
                        }
                    }
                    Ok(None) => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }

                if last_report.elapsed() >= Duration::from_secs(1) {
                    let sec = last_report.elapsed().as_secs_f64();
                    let fps = frames_streamed as f64 / sec;
                    let mbps = (bytes_streamed as f64 * 8.0) / (sec * 1_000_000.0);
                    fps_holder.store((fps * 10.0) as u64, Ordering::Relaxed);
                    mbps_holder.store((mbps * 10.0) as u64, Ordering::Relaxed);
                    frames_streamed = 0;
                    bytes_streamed = 0;
                    last_report = Instant::now();
                }
            }
        });

        workers.push(vid_worker);
    }

    *state.mode.lock().unwrap() = "Host".into();
    *state.peer_device.lock().unwrap() = Some(target_ip.clone());
    *state.hook_manager.lock().unwrap() = Some(hook_manager);
    *state.active_workers.lock().unwrap() = workers;
    state.is_running.store(true, Ordering::SeqCst);

    Ok(format!("Host mode active -> streaming to {target_ip}"))
}

#[command]
async fn start_client(
    bind_port: u16,
    state: State<'_, AppState>,
) -> Result<String, String> {
    stop_service_internal(&state);

    let input_bind: SocketAddr = format!("0.0.0.0:{bind_port}")
        .parse()
        .map_err(|e| format!("Invalid bind address: {e}"))?;
    let video_bind: SocketAddr = format!("0.0.0.0:{}", bind_port + 1)
        .parse()
        .map_err(|e| format!("Invalid bind address: {e}"))?;

    info!("Starting internal Client engine on input={}, video={}", input_bind, video_bind);

    let mut input_receiver = UdpInputReceiver::bind(input_bind)
        .await
        .map_err(|e| format!("Failed to bind input receiver: {e}"))?;

    let emulator = InputEmulator::new();
    let is_running_in = state.is_running.clone();
    let rtt_holder = state.live_rtt_us.clone();

    let input_worker = tokio::spawn(async move {
        while is_running_in.load(Ordering::SeqCst) {
            match input_receiver.recv_event().await {
                Ok((header, event, _peer)) => {
                    let now_us = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64;
                    let latency_us = now_us.saturating_sub(header.timestamp_us);
                    rtt_holder.store(latency_us, Ordering::Relaxed);
                    let _ = emulator.inject(&event);
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        }
    });

    let is_running_vid = state.is_running.clone();
    let fps_holder = state.live_fps_x10.clone();

    let video_worker = tokio::spawn(async move {
        let mut video_receiver = match VideoReceiver::bind(video_bind).await {
            Ok(r) => r,
            Err(e) => {
                error!("Failed to bind video receiver: {:?}", e);
                return;
            }
        };

        let decoder_config = VideoDecoderConfig::default();
        let mut decoder = match HardwareVideoDecoder::new(decoder_config) {
            Ok(d) => d,
            Err(e) => {
                error!("Failed to init decoder: {:?}", e);
                return;
            }
        };

        let mut frames = 0u64;
        let mut last_rep = Instant::now();

        while is_running_vid.load(Ordering::SeqCst) {
            match video_receiver.recv_frame().await {
                Ok((frame_id, _key, timestamp_us, data)) => {
                    frames += 1;
                    let _ = decoder.decode_frame(frame_id, timestamp_us, &data);

                    if last_rep.elapsed() >= Duration::from_secs(1) {
                        let fps = frames as f64 / last_rep.elapsed().as_secs_f64();
                        fps_holder.store((fps * 10.0) as u64, Ordering::Relaxed);
                        frames = 0;
                        last_rep = Instant::now();
                    }
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    });

    *state.mode.lock().unwrap() = "Client".into();
    *state.peer_device.lock().unwrap() = Some(format!("0.0.0.0:{bind_port}"));
    *state.active_workers.lock().unwrap() = vec![input_worker, video_worker];
    state.is_running.store(true, Ordering::SeqCst);

    Ok(format!("Client mode active -> listening on port {bind_port}"))
}

#[command]
fn stop_service(state: State<'_, AppState>) -> Result<String, String> {
    stop_service_internal(&state);
    Ok("Service stopped".into())
}

#[command]
fn set_screen_edge(edge: String, state: State<'_, AppState>) -> Result<String, String> {
    info!("Setting screen transition edge to {}", edge);
    *state.screen_edge.lock().unwrap() = edge.clone();
    let screen_edge = match edge.as_str() {
        "Left" => input::ScreenEdge::Left,
        "Right" => input::ScreenEdge::Right,
        "Top" => input::ScreenEdge::Top,
        "Bottom" => input::ScreenEdge::Bottom,
        _ => input::ScreenEdge::Right,
    };
    input::set_active_edge(screen_edge);
    Ok(format!("Edge set to {}", edge))
}

#[command]
fn toggle_virtual_display(enable: bool, state: State<'_, AppState>) -> Result<bool, String> {
    if enable {
        state.display_manager.plug_display().map_err(|e| e.to_string())?;
    } else {
        state.display_manager.unplug_display().map_err(|e| e.to_string())?;
    }
    Ok(state.display_manager.is_plugged())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let discovered_cache = Arc::new(Mutex::new(HashMap::new()));
    let discovered_cache_clone = discovered_cache.clone();

    let state = AppState {
        is_running: Arc::new(AtomicBool::new(false)),
        mode: Mutex::new("Idle".into()),
        peer_device: Mutex::new(None),
        screen_edge: Mutex::new("Right".into()),
        display_manager: VirtualDisplayManager::new(),
        hook_manager: Mutex::new(None),
        active_workers: Mutex::new(Vec::new()),
        live_rtt_us: Arc::new(AtomicU64::new(0)),
        live_fps_x10: Arc::new(AtomicU64::new(0)),
        live_mbps_x10: Arc::new(AtomicU64::new(0)),
        discovered_devices: discovered_cache,
    };

    let my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "NerveDesk-PC".into());
    let my_name_emitter = my_name.clone();
    let my_name_listener = my_name.clone();

    let sw = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN) } as u32;
    let sh = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN) } as u32;

    tauri::async_runtime::spawn(async move {
        let beacon = DiscoveryBeacon::new(my_name_emitter, DeviceRole::Host, 48100, 48101, sw, sh);
        if let Ok(emitter) = DiscoveryBeaconEmitter::new(beacon).await {
            let _ = emitter.spawn_loop(Duration::from_secs(1));
        }
    });

    tauri::async_runtime::spawn(async move {
        if let Ok(listener) = DiscoveryListener::bind().await {
            loop {
                if let Ok((beacon, peer_addr)) = listener.recv_beacon().await {
                    let ip = peer_addr.ip().to_string();
                    if beacon.device_name != my_name_listener {
                        if let Ok(mut cache) = discovered_cache_clone.lock() {
                            cache.insert(
                                ip.clone(),
                                DiscoveredDevice {
                                    name: beacon.device_name,
                                    role: format!("{:?}", beacon.role),
                                    ip,
                                    input_port: beacon.input_port,
                                    video_port: beacon.video_port,
                                    screen_width: beacon.screen_width,
                                    screen_height: beacon.screen_height,
                                },
                            );
                        }
                    }
                }
            }
        }
    });

    if let Err(e) = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_my_ip,
            get_metrics,
            discover_devices,
            start_host,
            start_client,
            stop_service,
            set_screen_edge,
            toggle_virtual_display
        ])
        .run(tauri::generate_context!())
    {
        unsafe {
            use windows::core::HSTRING;
            use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
            let msg = format!(
                "Failed to launch NerveDesk GUI: {}\n\nNote: If WebView2 is missing, install Microsoft Edge WebView2 Runtime.",
                e
            );
            let _ = MessageBoxW(
                None,
                &HSTRING::from(msg),
                &HSTRING::from("NerveDesk Error"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}