use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{command, Manager, State};
use tracing::{error, info, warn};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureSettings {
    pub keyboard_sharing: bool,
    pub clipboard_sharing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardPacket {
    pub token: String,
    pub text: String,
}

pub struct AppState {
    pub is_running: Arc<AtomicBool>,
    pub mode: Mutex<String>,
    pub peer_device: Arc<Mutex<Option<String>>>,
    pub screen_edge: Mutex<String>,
    pub display_manager: VirtualDisplayManager,
    pub hook_manager: Mutex<Option<InputHookManager>>,
    pub active_workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    pub live_rtt_us: Arc<AtomicU64>,
    pub live_fps_x10: Arc<AtomicU64>,
    pub live_mbps_x10: Arc<AtomicU64>,
    pub discovered_devices: Arc<Mutex<HashMap<String, DiscoveredDevice>>>,
    pub keyboard_sharing: Arc<AtomicBool>,
    pub clipboard_sharing: Arc<AtomicBool>,
    pub paired_security_key: Arc<Mutex<Option<String>>>,
    pub paired_peer_ip: Arc<Mutex<Option<String>>>,
    pub clipboard_hash: Arc<AtomicU64>,
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
fn get_device_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Desktop-PC".into())
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
    let mut peer = state.peer_device.lock().unwrap().clone();

    if let Some(ref p) = peer {
        if p.parse::<std::net::IpAddr>().is_ok() {
            if let Ok(guard) = state.discovered_devices.lock() {
                if let Some(dev) = guard.get(p) {
                    peer = Some(dev.name.clone());
                    *state.peer_device.lock().unwrap() = Some(dev.name.clone());
                }
            }
        }
    }

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
async fn pair_device(
    target_name: String,
    security_key: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let clean_name = target_name.trim();
    if clean_name.is_empty() {
        return Err("Device name cannot be empty".into());
    }
    if security_key.trim().is_empty() {
        return Err("Security key cannot be empty".into());
    }

    if let Ok(mut key_guard) = state.paired_security_key.lock() {
        *key_guard = Some(security_key.trim().to_string());
    }

    let my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Desktop-PC".into());
    let sw = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN) } as u32;
    let sh = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN) } as u32;
    let beacon = DiscoveryBeacon::new(my_name, DeviceRole::Host, 48100, 48101, sw, sh);
    if let Ok(emitter) = DiscoveryBeaconEmitter::new(beacon).await {
        let _ = emitter.emit_beacon().await;
    }

    start_host(clean_name.to_string(), false, state).await
}

#[command]
async fn start_host(
    target_ip: String,
    stream_video: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    stop_service_internal(&state);

    let host_part = if let Some((h, _)) = target_ip.split_once(':') {
        h
    } else {
        target_ip.as_str()
    };

    let resolved_ip = if let Ok(ip) = host_part.parse::<std::net::IpAddr>() {
        ip.to_string()
    } else {
        let mut found_ip = None;
        if let Ok(guard) = state.discovered_devices.lock() {
            for d in guard.values() {
                if d.name.eq_ignore_ascii_case(host_part) {
                    found_ip = Some(d.ip.clone());
                    break;
                }
            }
        }
        if let Some(ip) = found_ip {
            ip
        } else if let Ok(mut addrs) = tokio::net::lookup_host(format!("{host_part}:48100")).await {
            if let Some(addr) = addrs.next() {
                addr.ip().to_string()
            } else {
                return Err(format!("Could not resolve device name: {host_part}"));
            }
        } else {
            return Err(format!("Device not reachable: {host_part}"));
        }
    };

    if let Ok(mut ip_guard) = state.paired_peer_ip.lock() {
        *ip_guard = Some(resolved_ip.clone());
    }

    let target_addr: SocketAddr = format!("{resolved_ip}:48100")
        .parse()
        .map_err(|e| format!("Invalid target address: {e}"))?;

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

    let _ = input_sender.send_event(&InputEvent::Heartbeat).await;

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
                bitrate_bps: 10_000_000,
                keyframe_interval: 120,
            };
            let encoder = match HardwareVideoEncoder::new(encoder_config) {
                Ok(enc) => enc,
                Err(e) => {
                    error!("Encoder error: {:?}", e);
                    return;
                }
            };

            let mut frame_count = 0u64;
            let mut total_bytes = 0u64;
            let mut last_report = Instant::now();

            while is_running_vid.load(Ordering::SeqCst) {
                let start_frame = Instant::now();
                let now_us = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_micros() as u64;

                match duplicator.acquire_frame() {
                    Ok(Some(frame)) => {
                        let is_key = frame_count % 120 == 0;
                        match encoder.encode_frame(&frame.texture, now_us, is_key) {
                            Ok(encoded_frame) => {
                                if let Ok(sent_bytes) = video_sender.send_frame(&encoded_frame).await {
                                    total_bytes += sent_bytes as u64;
                                    frame_count += 1;
                                }
                            }
                            Err(_) => {}
                        }
                    }
                    Ok(None) => {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(16)).await;
                    }
                }

                if last_report.elapsed() >= Duration::from_secs(1) {
                    let elapsed = last_report.elapsed().as_secs_f64();
                    let fps = (frame_count as f64) / elapsed;
                    let mbps = ((total_bytes * 8) as f64) / (elapsed * 1_000_000.0);

                    fps_holder.store((fps * 10.0) as u64, Ordering::Relaxed);
                    mbps_holder.store((mbps * 10.0) as u64, Ordering::Relaxed);

                    frame_count = 0;
                    total_bytes = 0;
                    last_report = Instant::now();
                }

                let frame_time = start_frame.elapsed();
                if frame_time < Duration::from_millis(16) {
                    tokio::time::sleep(Duration::from_millis(16) - frame_time).await;
                }
            }
        });

        workers.push(vid_worker);
    }

    *state.hook_manager.lock().unwrap() = Some(hook_manager);
    *state.active_workers.lock().unwrap() = workers;
    state.is_running.store(true, Ordering::SeqCst);
    *state.mode.lock().unwrap() = if stream_video { "Cast & KVM".into() } else { "KVM Active".into() };

    let display_name = if let Ok(guard) = state.discovered_devices.lock() {
        if let Some(d) = guard.get(&target_addr.ip().to_string()) {
            d.name.clone()
        } else {
            target_ip.clone()
        }
    } else {
        target_ip.clone()
    };
    *state.peer_device.lock().unwrap() = Some(display_name);

    Ok(format!("Host service active -> {}", target_addr))
}

#[command]
async fn start_client(
    bind_port: u16,
    state: State<'_, AppState>,
) -> Result<String, String> {
    stop_service_internal(&state);

    let input_bind: SocketAddr = format!("0.0.0.0:{bind_port}")
        .parse()
        .map_err(|e| format!("Invalid input port: {e}"))?;

    let video_bind: SocketAddr = format!("0.0.0.0:{}", bind_port + 1)
        .parse()
        .map_err(|e| format!("Invalid video port: {e}"))?;

    info!("Starting internal Client engine on input={}, video={}", input_bind, video_bind);

    let mut input_receiver = UdpInputReceiver::bind(input_bind)
        .await
        .map_err(|e| format!("Failed to bind input receiver: {e}"))?;

    let emulator = InputEmulator::new();
    let is_running_in = state.is_running.clone();
    let rtt_holder = state.live_rtt_us.clone();
    let kb_sharing = state.keyboard_sharing.clone();
    let peer_ip_ref = state.paired_peer_ip.clone();

    let input_worker = tokio::spawn(async move {
        while is_running_in.load(Ordering::SeqCst) {
            match input_receiver.recv_event().await {
                Ok((header, event, peer)) => {
                    if let Ok(guard) = peer_ip_ref.lock() {
                        if let Some(ref allowed_ip) = *guard {
                            if peer.ip().to_string() != *allowed_ip && peer.ip().to_string() != "127.0.0.1" {
                                continue;
                            }
                        }
                    }

                    if let InputEvent::Keyboard { .. } = event {
                        if !kb_sharing.load(Ordering::Relaxed) {
                            continue;
                        }
                    }

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
                Ok((frame_id, _is_keyframe, timestamp_us, frame_data)) => {
                    if decoder.decode_frame(frame_id, timestamp_us, &frame_data).is_ok() {
                        frames += 1;
                    }
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            }

            if last_rep.elapsed() >= Duration::from_secs(1) {
                let el = last_rep.elapsed().as_secs_f64();
                let fps = (frames as f64) / el;
                fps_holder.store((fps * 10.0) as u64, Ordering::Relaxed);
                frames = 0;
                last_rep = Instant::now();
            }
        }
    });

    *state.active_workers.lock().unwrap() = vec![input_worker, video_worker];
    state.is_running.store(true, Ordering::SeqCst);
    *state.mode.lock().unwrap() = "Client Listening".into();
    *state.peer_device.lock().unwrap() = Some(format!("0.0.0.0:{bind_port}"));

    Ok(format!("Client receiver active on port {}", bind_port))
}

#[command]
fn stop_service(state: State<'_, AppState>) -> Result<String, String> {
    stop_service_internal(&state);
    Ok("Engine safely terminated".into())
}

#[command]
fn set_screen_edge(edge: String, state: State<'_, AppState>) -> Result<String, String> {
    let screen_edge = match edge.as_str() {
        "Left" => input::ScreenEdge::Left,
        "Right" => input::ScreenEdge::Right,
        "Top" => input::ScreenEdge::Top,
        "Bottom" => input::ScreenEdge::Bottom,
        _ => return Err("Invalid edge value. Use Left, Right, Top, or Bottom".into()),
    };

    let (mut client_w, mut client_h) = (0, 0);
    if let Ok(guard) = state.discovered_devices.lock() {
        if let Ok(peer_guard) = state.peer_device.lock() {
            if let Some(ref peer) = *peer_guard {
                if let Some(dev) = guard.get(peer) {
                    client_w = dev.screen_width as i32;
                    client_h = dev.screen_height as i32;
                }
            }
        }
    }

    input::set_active_edge_and_resolution(screen_edge, client_w, client_h);
    *state.screen_edge.lock().unwrap() = edge.clone();
    info!("Active crossing boundary adjusted to: {}", edge);
    Ok(format!("Screen edge set to {}", edge))
}

#[command]
fn toggle_virtual_display(state: State<'_, AppState>) -> Result<bool, String> {
    if state.display_manager.is_plugged() {
        state.display_manager.unplug_display().map_err(|e| e.to_string())?;
        Ok(false)
    } else {
        state.display_manager.plug_display().map_err(|e| e.to_string())?;
        Ok(true)
    }
}

#[command]
fn set_keyboard_sharing_enabled(enabled: bool, state: State<'_, AppState>) {
    state.keyboard_sharing.store(enabled, Ordering::SeqCst);
    input::set_keyboard_sharing(enabled);
}

#[command]
fn set_clipboard_sharing_enabled(enabled: bool, state: State<'_, AppState>) {
    state.clipboard_sharing.store(enabled, Ordering::SeqCst);
}

#[command]
fn get_feature_settings(state: State<'_, AppState>) -> FeatureSettings {
    FeatureSettings {
        keyboard_sharing: state.keyboard_sharing.load(Ordering::SeqCst),
        clipboard_sharing: state.clipboard_sharing.load(Ordering::SeqCst),
    }
}

#[command]
fn open_display_settings() {
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "ms-settings:display"])
        .spawn();
}

#[command]
async fn start_virtual_display_cast(
    target_name: String,
    extend_mode: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if extend_mode {
        let _ = state.display_manager.plug_display();
    }
    start_host(target_name, true, state).await
}

#[command]
fn stop_virtual_display_cast(state: State<'_, AppState>) {
    let _ = state.display_manager.unplug_display();
    stop_service_internal(&state);
}

pub fn run() {
    let state = AppState {
        is_running: Arc::new(AtomicBool::new(false)),
        mode: Mutex::new("Idle".into()),
        peer_device: Arc::new(Mutex::new(None)),
        screen_edge: Mutex::new("Right".into()),
        display_manager: VirtualDisplayManager::new(),
        hook_manager: Mutex::new(None),
        active_workers: Mutex::new(Vec::new()),
        live_rtt_us: Arc::new(AtomicU64::new(0)),
        live_fps_x10: Arc::new(AtomicU64::new(0)),
        live_mbps_x10: Arc::new(AtomicU64::new(0)),
        discovered_devices: Arc::new(Mutex::new(HashMap::new())),
        keyboard_sharing: Arc::new(AtomicBool::new(true)),
        clipboard_sharing: Arc::new(AtomicBool::new(true)),
        paired_security_key: Arc::new(Mutex::new(None)),
        paired_peer_ip: Arc::new(Mutex::new(None)),
        clipboard_hash: Arc::new(AtomicU64::new(0)),
    };

    let discovered_for_disc = state.discovered_devices.clone();
    let peer_device_for_disc = state.peer_device.clone();
    let discovered_for_rx = state.discovered_devices.clone();
    let peer_device_for_rx = state.peer_device.clone();
    let live_rtt_for_rx = state.live_rtt_us.clone();
    let paired_ip_for_rx = state.paired_peer_ip.clone();
    let keyboard_sharing_for_rx = state.keyboard_sharing.clone();

    tauri::async_runtime::spawn(async move {
        if let Ok(listener) = DiscoveryListener::bind().await {
            loop {
                if let Ok((beacon, peer_addr)) = listener.recv_beacon().await {
                    let my_name = std::env::var("COMPUTERNAME").unwrap_or_default();
                    if !my_name.is_empty() && beacon.device_name.eq_ignore_ascii_case(&my_name) {
                        continue;
                    }
                    let ip = peer_addr.ip().to_string();
                    if let Ok(mut map) = discovered_for_disc.lock() {
                        let role_str = match beacon.role {
                            DeviceRole::Host => "Host",
                            DeviceRole::Client => "Client",
                        };
                        if let Some(existing) = map.get_mut(&ip) {
                            existing.name = beacon.device_name.clone();
                            existing.role = role_str.to_string();
                            existing.screen_width = beacon.screen_width;
                            existing.screen_height = beacon.screen_height;
                        } else {
                            map.insert(
                                ip.clone(),
                                DiscoveredDevice {
                                    name: beacon.device_name.clone(),
                                    role: role_str.to_string(),
                                    ip: ip.clone(),
                                    input_port: beacon.input_port,
                                    video_port: beacon.video_port,
                                    screen_width: beacon.screen_width,
                                    screen_height: beacon.screen_height,
                                },
                            );
                        }
                        if let Ok(mut cur) = peer_device_for_disc.lock() {
                            if let Some(ref existing) = *cur {
                                if existing == &ip {
                                    *cur = Some(beacon.device_name);
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    tauri::async_runtime::spawn(async move {
        let bind_addr: SocketAddr = "0.0.0.0:48100".parse().unwrap();
        let mut receiver = match UdpInputReceiver::bind(bind_addr).await {
            Ok(r) => r,
            Err(e) => {
                warn!("Auto background receiver bind failed: {:?}", e);
                return;
            }
        };

        let emulator = InputEmulator::new();
        loop {
            match receiver.recv_event().await {
                Ok((header, event, peer)) => {
                    if let Ok(guard) = paired_ip_for_rx.lock() {
                        if let Some(ref allowed_ip) = *guard {
                            if peer.ip().to_string() != *allowed_ip && peer.ip().to_string() != "127.0.0.1" {
                                continue;
                            }
                        }
                    }

                    if let InputEvent::Keyboard { .. } = event {
                        if !keyboard_sharing_for_rx.load(Ordering::Relaxed) {
                            continue;
                        }
                    }

                    let now_us = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64;
                    let latency_us = now_us.saturating_sub(header.timestamp_us);
                    live_rtt_for_rx.store(latency_us, Ordering::Relaxed);

                    let peer_ip = peer.ip().to_string();
                    let peer_name = if let Ok(guard) = discovered_for_rx.lock() {
                        if let Some(d) = guard.get(&peer_ip) {
                            d.name.clone()
                        } else {
                            peer_ip.clone()
                        }
                    } else {
                        peer_ip.clone()
                    };

                    if let Ok(mut cur) = peer_device_for_rx.lock() {
                        if let Some(ref existing) = *cur {
                            if existing.parse::<std::net::IpAddr>().is_ok() && peer_name != peer_ip {
                                *cur = Some(peer_name);
                            } else if cur.as_deref() != Some(&peer_name) && peer_name != peer_ip {
                                *cur = Some(peer_name);
                            }
                        } else {
                            *cur = Some(peer_name);
                        }
                    }

                    let _ = emulator.inject(&event);
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        }
    });

    let clipboard_sharing_for_srv = state.clipboard_sharing.clone();
    let paired_key_for_srv = state.paired_security_key.clone();
    let paired_ip_for_srv = state.paired_peer_ip.clone();
    let clipboard_hash_for_srv = state.clipboard_hash.clone();

    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::bind("0.0.0.0:48102").await {
            Ok(l) => l,
            Err(e) => {
                warn!("Clipboard TCP listener failed to bind: {:?}", e);
                return;
            }
        };

        while let Ok((mut socket, peer_addr)) = listener.accept().await {
            if !clipboard_sharing_for_srv.load(Ordering::Relaxed) {
                continue;
            }

            if let Ok(guard) = paired_ip_for_srv.lock() {
                if let Some(ref allowed_ip) = *guard {
                    if peer_addr.ip().to_string() != *allowed_ip && peer_addr.ip().to_string() != "127.0.0.1" {
                        continue;
                    }
                }
            }

            let hash_ref = clipboard_hash_for_srv.clone();
            let key_ref = paired_key_for_srv.clone();

            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut len_buf = [0u8; 4];
                if socket.read_exact(&mut len_buf).await.is_err() {
                    return;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                if len == 0 || len > 2 * 1024 * 1024 {
                    return;
                }
                let mut payload_buf = vec![0u8; len];
                if socket.read_exact(&mut payload_buf).await.is_err() {
                    return;
                }

                if let Ok(clip_packet) = serde_json::from_slice::<ClipboardPacket>(&payload_buf) {
                    if let Ok(guard) = key_ref.lock() {
                        if let Some(ref key) = *guard {
                            if !key.is_empty() && clip_packet.token != *key {
                                return;
                            }
                        }
                    }

                    let payload = clipboard::ClipboardPayload::Text(clip_packet.text.clone());
                    let h = payload.compute_hash();
                    if hash_ref.swap(h, Ordering::SeqCst) != h {
                        let _ = clipboard::ClipboardWriter::write_text(&clip_packet.text);
                    }
                }
            });
        }
    });

    let (clip_tx, clip_rx) = crossbeam_channel::unbounded::<clipboard::ClipboardPayload>();
    let clipboard_hash_for_listener = state.clipboard_hash.clone();
    let clipboard_sharing_for_tx = state.clipboard_sharing.clone();
    let paired_key_for_tx = state.paired_security_key.clone();
    let paired_ip_for_tx = state.paired_peer_ip.clone();

    if let Ok(listener) = clipboard::ClipboardListener::start(clip_tx, clipboard_hash_for_listener) {
        std::mem::forget(listener);
    }

    tauri::async_runtime::spawn(async move {
        while let Ok(payload) = clip_rx.recv() {
            if !clipboard_sharing_for_tx.load(Ordering::Relaxed) {
                continue;
            }

            let target_ip = {
                if let Ok(guard) = paired_ip_for_tx.lock() {
                    guard.clone()
                } else {
                    None
                }
            };

            if let Some(ip) = target_ip {
                let token = {
                    if let Ok(guard) = paired_key_for_tx.lock() {
                        guard.clone().unwrap_or_default()
                    } else {
                        String::new()
                    }
                };

                let text = match payload {
                    clipboard::ClipboardPayload::Text(t) => t,
                    _ => continue,
                };

                let packet = ClipboardPacket { token, text };
                if let Ok(bytes) = serde_json::to_vec(&packet) {
                    let len = (bytes.len() as u32).to_be_bytes();
                    let target_addr = format!("{}:48102", ip);
                    tokio::spawn(async move {
                        use tokio::io::AsyncWriteExt;
                        if let Ok(stream_res) = tokio::time::timeout(
                            Duration::from_secs(1),
                            tokio::net::TcpStream::connect(&target_addr),
                        )
                        .await
                        {
                            if let Ok(mut stream) = stream_res {
                                let _ = stream.write_all(&len).await;
                                let _ = stream.write_all(&bytes).await;
                                let _ = stream.flush().await;
                            }
                        }
                    });
                }
            }
        }
    });

    if let Err(e) = tauri::Builder::default()
        .manage(state)
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_theme(Some(tauri::Theme::Dark));
                #[cfg(target_os = "windows")]
                {
                    use windows::Win32::Foundation::HWND;
                    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE};

                    if let Ok(raw_hwnd) = window.hwnd() {
                        let hwnd = HWND(raw_hwnd.0 as *mut std::ffi::c_void);
                        let dark: u32 = 1;
                        unsafe {
                            let _ = DwmSetWindowAttribute(
                                hwnd,
                                DWMWA_USE_IMMERSIVE_DARK_MODE,
                                &dark as *const _ as _,
                                std::mem::size_of::<u32>() as u32,
                            );
                            let _ = DwmSetWindowAttribute(
                                hwnd,
                                DWMWINDOWATTRIBUTE(19),
                                &dark as *const _ as _,
                                std::mem::size_of::<u32>() as u32,
                            );
                            let caption_color: u32 = 0x00141212;
                            let _ = DwmSetWindowAttribute(
                                hwnd,
                                DWMWINDOWATTRIBUTE(35),
                                &caption_color as *const _ as _,
                                std::mem::size_of::<u32>() as u32,
                            );
                        }
                    }
                }
            }

            let quit_i = MenuItem::with_id(app, "quit", "Exit NerveDesk", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "Open Window", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            let mut builder = TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("NerveDesk - KVM & Virtual Display")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => {
                        app.exit(0);
                    }
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.unminimize();
                            let _ = window.set_focus();
                        }
                    }
                });

            if let Some(icon) = app.default_window_icon() {
                builder = builder.icon(icon.clone());
            }

            let _tray = builder.build(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_device_name,
            get_my_ip,
            get_metrics,
            discover_devices,
            pair_device,
            start_host,
            start_client,
            stop_service,
            set_screen_edge,
            toggle_virtual_display,
            set_keyboard_sharing_enabled,
            set_clipboard_sharing_enabled,
            get_feature_settings,
            open_display_settings,
            start_virtual_display_cast,
            stop_virtual_display_cast
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
