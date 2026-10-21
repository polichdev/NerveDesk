use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{command, Manager, State};
use tracing::{error, info, warn};

use capture::{DxgiCaptureConfig, VirtualDisplayManager};
use codec::{HardwareVideoDecoder, HardwareVideoEncoder, VideoDecoderConfig, VideoEncoderConfig};
use common::InputEvent;
use input::{InputEmulator, InputHookManager};
use net::{
    DeviceRole, DiscoveryBeacon, DiscoveryBeaconEmitter, DiscoveryListener,
    UdpInputReceiver, UdpInputSender, VideoReceiver, VideoSender,
};

mod viewer;
use viewer::ProjectionViewer;
mod mirror_stream;
mod mirror_worker;
mod window_capture;

mod badge;
use badge::DragBadge;

mod driver_install;
pub use driver_install::handle_cli as handle_driver_cli;

pub mod mobile_server;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMetrics {
    pub mode: String,
    pub is_active: bool,
    pub rtt_ms: f64,
    pub video_fps: f64,
    pub bandwidth_mbps: f64,
    pub peer_device: Option<String>,
    pub virtual_display_active: bool,
    pub projection_active: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileRemoteInfo {
    pub is_active: bool,
    pub ip: String,
    pub port: u16,
    pub url: String,
    pub qr_svg: String,
    pub pin: String,
}

pub struct AppState {
    pub is_kvm_active: Arc<AtomicBool>,
    pub is_projecting: Arc<AtomicBool>,
    pub mode: Mutex<String>,
    pub peer_device: Arc<Mutex<Option<String>>>,
    pub screen_edge: Mutex<String>,
    pub display_manager: VirtualDisplayManager,
    pub hook_manager: Mutex<Option<InputHookManager>>,
    pub kvm_workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    pub projection_workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    pub live_rtt_us: Arc<AtomicU64>,
    pub live_fps_x10: Arc<AtomicU64>,
    pub live_mbps_x10: Arc<AtomicU64>,
    pub discovered_devices: Arc<Mutex<HashMap<String, DiscoveredDevice>>>,
    pub keyboard_sharing: Arc<AtomicBool>,
    pub clipboard_sharing: Arc<AtomicBool>,
    pub paired_security_key: Arc<Mutex<Option<String>>>,
    pub paired_peer_ip: Arc<Mutex<Option<String>>>,
    pub clipboard_hash: Arc<AtomicU64>,
    pub projection_viewer: Arc<ProjectionViewer>,
    pub projection_cancel: Arc<AtomicBool>,
    pub projection_generation: Arc<AtomicU64>,
    pub file_transfer_enabled: Arc<AtomicBool>,
    pub drag_badge: Arc<DragBadge>,
    pub mobile_server: Arc<mobile_server::MobileServer>,
}

fn update_mode_string(state: &AppState) {
    let kvm = state.is_kvm_active.load(Ordering::SeqCst);
    let proj = state.is_projecting.load(Ordering::SeqCst);
    let m = match (kvm, proj) {
        (true, true) => "KVM & Projection Active",
        (true, false) => "KVM Active",
        (false, true) => "Projection Active",
        (false, false) => "Idle",
    };
    *state.mode.lock().unwrap() = m.into();
}

fn stop_kvm_internal(state: &AppState) {
    state.is_kvm_active.store(false, Ordering::SeqCst);
    if let Ok(mut guard) = state.hook_manager.lock() {
        if let Some(mut hook) = guard.take() {
            hook.stop();
        }
    }
    if let Ok(mut workers) = state.kvm_workers.lock() {
        for w in workers.drain(..) {
            w.abort();
        }
    }
    state.live_rtt_us.store(0, Ordering::Relaxed);
    update_mode_string(state);
}

fn stop_projection_internal(state: &AppState) {
    state.projection_cancel.store(true, Ordering::SeqCst);
    state.projection_generation.fetch_add(1, Ordering::SeqCst);
    state.is_projecting.store(false, Ordering::SeqCst);
    state.projection_viewer.stop();
    let _ = state.display_manager.unplug_display();
    if let Ok(mut workers) = state.projection_workers.lock() {
        for w in workers.drain(..) {
            w.abort();
        }
    }
    state.live_fps_x10.store(0, Ordering::Relaxed);
    state.live_mbps_x10.store(0, Ordering::Relaxed);
    update_mode_string(state);
}

async fn resolve_target_ip(target_ip: &str, state: &AppState) -> Result<String, String> {
    let host_part = if let Some((h, _)) = target_ip.split_once(':') {
        h
    } else {
        target_ip
    };

    if let Ok(ip) = host_part.parse::<std::net::IpAddr>() {
        return Ok(ip.to_string());
    }

    if let Ok(guard) = state.discovered_devices.lock() {
        for d in guard.values() {
            if d.name.eq_ignore_ascii_case(host_part) {
                return Ok(d.ip.clone());
            }
        }
    }

    if let Ok(mut addrs) = tokio::net::lookup_host(format!("{host_part}:48100")).await {
        if let Some(addr) = addrs.next() {
            return Ok(addr.ip().to_string());
        }
    }

    Err(format!("Device not reachable: {host_part}"))
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
    let is_active = state.is_kvm_active.load(Ordering::SeqCst) || state.is_projecting.load(Ordering::SeqCst);
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
        projection_active: state.is_projecting.load(Ordering::SeqCst),
        screen_edge: edge,
    }
}

fn get_screen_dimensions() -> (u32, u32) {
    unsafe {
        use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC, GetDeviceCaps, DESKTOPHORZRES, DESKTOPVERTRES};
        use windows::Win32::Foundation::HWND;
        let hdc = GetDC(HWND::default());
        let mut w = 0;
        let mut h = 0;
        if !hdc.is_invalid() {
            w = GetDeviceCaps(hdc, DESKTOPHORZRES) as u32;
            h = GetDeviceCaps(hdc, DESKTOPVERTRES) as u32;
            let _ = ReleaseDC(HWND::default(), hdc);
        }
        if w == 0 || h == 0 {
            w = windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN) as u32;
            h = windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN) as u32;
        }
        (if w > 0 { w } else { 1920 }, if h > 0 { h } else { 1080 })
    }
}

#[command]
async fn discover_devices(state: State<'_, AppState>) -> Result<Vec<DiscoveredDevice>, String> {
    let my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Host-PC".into());
    let (sw, sh) = get_screen_dimensions();
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
    let (sw, sh) = get_screen_dimensions();
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
    stop_kvm_internal(&state);

    let resolved_ip = resolve_target_ip(&target_ip, &state).await?;

    if let Ok(mut ip_guard) = state.paired_peer_ip.lock() {
        *ip_guard = Some(resolved_ip.clone());
    }

    let target_addr: SocketAddr = format!("{resolved_ip}:48100")
        .parse()
        .map_err(|e| format!("Invalid target address: {e}"))?;

    info!("Starting internal Host KVM engine -> input={}", target_addr);

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

    let is_running_in = state.is_kvm_active.clone();
    is_running_in.store(true, Ordering::SeqCst);
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(
            windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow(),
        );
    }

    let my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Desktop-PC".into());
    let _ = input_sender
        .send_event(&InputEvent::Heartbeat {
            device_name: my_name.clone(),
        })
        .await;

    let heartbeat_name = my_name.clone();
    let is_file_transfer_enabled = state.file_transfer_enabled.clone();
    let paired_key_for_input = state.paired_security_key.clone();
    let target_file_addr = format!("{}:{}", target_addr.ip(), net::FILE_TRANSFER_PORT);

    let input_worker = tokio::spawn(async move {
        let mut last_hb = Instant::now();
        while is_running_in.load(Ordering::SeqCst) {
            match event_rx.try_recv() {
                Ok(event) => {
                    if let InputEvent::FileDrag { active, .. } = event {
                        if active && is_file_transfer_enabled.load(Ordering::Relaxed) {
                            let dragged_files = net::get_dragged_or_selected_files();
                            if let Some(first) = dragged_files.first() {
                                let file_name = first
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("file")
                                    .to_string();
                                let _ = input_sender
                                    .send_event(&InputEvent::FileDrag {
                                        active: true,
                                        file_name,
                                    })
                                    .await;
                                let token = if let Ok(guard) = paired_key_for_input.lock() {
                                    guard.clone().unwrap_or_default()
                                } else {
                                    String::new()
                                };
                                let addr = target_file_addr.clone();
                                tokio::spawn(async move {
                                    let _ = net::FileTransferClient::send_files(&addr, &token, &dragged_files).await;
                                });
                            }
                        } else {
                            let _ = input_sender
                                .send_event(&InputEvent::FileDrag {
                                    active: false,
                                    file_name: String::new(),
                                })
                                .await;
                        }
                    } else {
                        let _ = input_sender.send_event(&event).await;
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    if last_hb.elapsed() >= Duration::from_secs(1) {
                        let _ = input_sender
                            .send_event(&InputEvent::Heartbeat {
                                device_name: heartbeat_name.clone(),
                            })
                            .await;
                        last_hb = Instant::now();
                    }
                    tokio::time::sleep(Duration::from_micros(500)).await;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }
    });

    *state.hook_manager.lock().unwrap() = Some(hook_manager);
    *state.kvm_workers.lock().unwrap() = vec![input_worker];

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

    if stream_video {
        let _ = start_virtual_display_cast(target_ip.clone(), false, None, state.clone()).await;
    }

    update_mode_string(&state);
    Ok(format!("Host service active -> {}", target_addr))
}

#[command]
fn list_capture_windows() -> Vec<window_capture::CaptureWindow> {
    window_capture::list_windows()
}

#[command]
async fn start_virtual_display_cast(
    target_name: String,
    extend_mode: bool,
    source_window: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    stop_projection_internal(&state);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let resolved_ip = resolve_target_ip(&target_name, &state).await?;

    if let Ok(mut ip_guard) = state.paired_peer_ip.lock() {
        if ip_guard.is_none() {
            *ip_guard = Some(resolved_ip.clone());
        }
    }

    let frame_listener = if extend_mode {
        Some(tokio::net::TcpListener::bind("127.0.0.1:48103")
            .await
            .map_err(|e| format!("Cannot listen for NerveDesk display frames: {e}"))?)
    } else {
        None
    };
    let mirror_capture_config = if extend_mode || source_window.is_some() {
        None
    } else {
        Some(DxgiCaptureConfig::primary_display().map_err(|e| e.to_string())?)
    };
    if extend_mode {
        std::process::Command::new("displayswitch.exe")
            .arg("/extend")
            .spawn()
            .map_err(|e| format!("Cannot extend Windows desktop: {e}"))?;
        for _ in 0..25 {
            if state.display_manager.is_plugged() { break; }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        state.display_manager.plug_display().map_err(|e| e.to_string())?;
    }

    let video_target: SocketAddr = format!("{}:48101", resolved_ip)
        .parse()
        .map_err(|e| format!("Invalid video target address: {e}"))?;

    state.projection_cancel.store(false, Ordering::SeqCst);
    let cancel_flag = state.projection_cancel.clone();
    let is_running_vid = state.is_projecting.clone();
    is_running_vid.store(true, Ordering::SeqCst);
    let fps_holder = state.live_fps_x10.clone();
    let mbps_holder = state.live_mbps_x10.clone();
    let generation = state.projection_generation.clone();
    let current_generation = generation.load(Ordering::SeqCst);

    let vid_worker = if let Some(listener) = frame_listener {
        tokio::spawn(async move {
            let sender = match VideoSender::new(video_target).await {
                Ok(sender) => sender,
                Err(e) => { error!("VideoSender error: {:?}", e); return; }
            };
            let encoder = match HardwareVideoEncoder::new(VideoEncoderConfig::default()) {
                Ok(encoder) => encoder,
                Err(e) => { error!("Encoder error: {:?}", e); return; }
            };
            let mut frame_count = 0u64;
            let mut byte_count = 0u64;
            let mut report = Instant::now();
            while !cancel_flag.load(Ordering::Relaxed) && is_running_vid.load(Ordering::SeqCst) {
                let (mut stream, _) = match tokio::time::timeout(Duration::from_millis(250), listener.accept()).await {
                    Ok(Ok(connection)) => connection,
                    Ok(Err(e)) => { warn!("IDD frame connection failed: {:?}", e); continue; }
                    Err(_) => continue,
                };
                loop {
                    if cancel_flag.load(Ordering::Relaxed) || !is_running_vid.load(Ordering::SeqCst) { break; }
                    let mut header = [0u8; 16];
                    if !matches!(tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut header)).await, Ok(Ok(_))) { break; }
                    let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
                    let width = u32::from_le_bytes(header[4..8].try_into().unwrap());
                    let height = u32::from_le_bytes(header[8..12].try_into().unwrap());
                    let len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
                    if magic != 0x3146444e || width == 0 || height == 0
                        || width > 7680 || height > 4320 || len != (width as usize) * (height as usize) * 4 {
                        warn!("Invalid IDD frame header");
                        break;
                    }
                    let mut pixels = vec![0u8; len];
                    if !matches!(tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut pixels)).await, Ok(Ok(_))) { break; }
                    let now_us = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as u64;
                    let keyframe = frame_count == 0 || frame_count % 60 == 0;
                    if let Ok(frame) = encoder.encode_bgra(width, height, &pixels, now_us, keyframe) {
                        if let Ok(bytes) = sender.send_frame(&frame).await {
                            frame_count += 1;
                            byte_count += bytes as u64;
                        }
                    }
                    if report.elapsed() >= Duration::from_secs(1) {
                        let elapsed = report.elapsed().as_secs_f64();
                        fps_holder.store((frame_count as f64 / elapsed * 10.0) as u64, Ordering::Relaxed);
                        mbps_holder.store((byte_count as f64 * 8.0 / elapsed / 100_000.0) as u64, Ordering::Relaxed);
                        frame_count = 0;
                        byte_count = 0;
                        report = Instant::now();
                    }
                }
            }
        })
    } else {
        let mirror_target: SocketAddr = format!("{}:48104", resolved_ip)
            .parse().map_err(|e| format!("Invalid mirror target address: {e}"))?;
        match mirror_worker::start(
            mirror_target,
            source_window,
            mirror_capture_config,
            cancel_flag,
            is_running_vid,
            fps_holder,
            mbps_holder,
            generation,
            current_generation,
        ).await {
            Ok(worker) => worker,
            Err(e) => {
                state.is_projecting.store(false, Ordering::SeqCst);
                return Err(e);
            }
        }
    };

    *state.projection_workers.lock().unwrap() = vec![vid_worker];
    update_mode_string(&state);

    Ok(format!("Projection active -> {}", video_target))
}

#[command]
fn stop_virtual_display_cast(state: State<'_, AppState>) {
    stop_projection_internal(&state);
}

#[command]
fn stop_service(state: State<'_, AppState>) -> Result<String, String> {
    stop_kvm_internal(&state);
    Ok("KVM stopped safely".into())
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
    let _ = state;
    Err("Use Windows Display Settings to enable or disable the NerveDesk display".into())
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
fn install_virtual_display_driver() -> Result<String, String> {
    driver_install::elevate("install")
}

#[command]
fn uninstall_virtual_display_driver() -> Result<String, String> {
    driver_install::elevate("remove")
}

#[command]
fn get_display_driver_status() -> bool {
    driver_install::is_installed()
}

#[command]
fn get_display_driver_package_status() -> bool {
    driver_install::package_available()
}

#[command]
async fn open_received_folder() -> Result<(), String> {
    let dir = net::get_default_transfer_dir();
    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
    Ok(())
}

#[command]
fn set_file_transfer_enabled(enabled: bool, state: State<'_, AppState>) {
    state.file_transfer_enabled.store(enabled, Ordering::SeqCst);
}

#[command]
fn get_autostart_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = std::process::Command::new("reg")
            .args(["query", "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run", "/v", "NerveDesk"])
            .output()
        {
            return output.status.success();
        }
    }
    false
}

#[command]
fn set_autostart_enabled(enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        if enabled {
            let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
            let exe_str = exe_path.to_str().ok_or_else(|| "Invalid exe path".to_string())?;
            let val = format!("\"{}\" --minimized", exe_str);
            let status = std::process::Command::new("reg")
                .args(["add", "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run", "/v", "NerveDesk", "/t", "REG_SZ", "/d", &val, "/f"])
                .status()
                .map_err(|e| e.to_string())?;
            if !status.success() {
                return Err("Failed to register autostart in Windows Registry".into());
            }
        } else {
            let _ = std::process::Command::new("reg")
                .args(["delete", "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run", "/v", "NerveDesk", "/f"])
                .status();
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(())
    }
}

#[command]
async fn send_file_to_peer(path: String, state: State<'_, AppState>) -> Result<String, String> {
    let target_ip = if let Ok(guard) = state.paired_peer_ip.lock() {
        guard.clone()
    } else {
        None
    };
    let ip = target_ip.ok_or_else(|| "No paired device connected".to_string())?;
    let token = if let Ok(guard) = state.paired_security_key.lock() {
        guard.clone().unwrap_or_default()
    } else {
        String::new()
    };
    let target_addr = format!("{}:{}", ip, net::FILE_TRANSFER_PORT);
    let bytes = net::FileTransferClient::send_files(&target_addr, &token, &[path])
        .await
        .map_err(|e| format!("File transfer failed: {e}"))?;
    Ok(format!("Transferred {bytes} bytes"))
}

#[command]
fn get_mobile_remote_info(state: State<'_, AppState>) -> MobileRemoteInfo {
    let ip = mobile_server::get_local_ip();
    let port = state.mobile_server.port();
    let is_active = state.mobile_server.is_active();
    let url = format!("http://{}:{}", ip, port);
    let qr_svg = mobile_server::generate_qr_svg(&url).unwrap_or_default();
    let pin = state.mobile_server.get_pin();
    MobileRemoteInfo {
        is_active,
        ip,
        port,
        url,
        qr_svg,
        pin,
    }
}

#[command]
fn regenerate_mobile_pin(state: State<'_, AppState>) -> MobileRemoteInfo {
    state.mobile_server.regenerate_pin();
    get_mobile_remote_info(state)
}

#[command]
fn toggle_mobile_remote(enable: bool, state: State<'_, AppState>) -> bool {
    if enable {
        state.mobile_server.start();
    } else {
        state.mobile_server.stop();
    }
    state.mobile_server.is_active()
}

#[command]
fn open_mobile_remote_url(state: State<'_, AppState>) {
    let ip = mobile_server::get_local_ip();
    let port = state.mobile_server.port();
    let url = format!("http://{}:{}", ip, port);
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", &url])
        .spawn();
}

pub fn run() {
    let projection_viewer = Arc::new(ProjectionViewer::new());
    let drag_badge = Arc::new(DragBadge::new());
    let mobile_server = Arc::new(mobile_server::MobileServer::new(5858));
    mobile_server.start();

    let state = AppState {
        is_kvm_active: Arc::new(AtomicBool::new(false)),
        is_projecting: Arc::new(AtomicBool::new(false)),
        mode: Mutex::new("Idle".into()),
        peer_device: Arc::new(Mutex::new(None)),
        screen_edge: Mutex::new("Right".into()),
        display_manager: VirtualDisplayManager::new(),
        hook_manager: Mutex::new(None),
        kvm_workers: Mutex::new(Vec::new()),
        projection_workers: Mutex::new(Vec::new()),
        live_rtt_us: Arc::new(AtomicU64::new(0)),
        live_fps_x10: Arc::new(AtomicU64::new(0)),
        live_mbps_x10: Arc::new(AtomicU64::new(0)),
        discovered_devices: Arc::new(Mutex::new(HashMap::new())),
        keyboard_sharing: Arc::new(AtomicBool::new(true)),
        clipboard_sharing: Arc::new(AtomicBool::new(true)),
        paired_security_key: Arc::new(Mutex::new(None)),
        paired_peer_ip: Arc::new(Mutex::new(None)),
        clipboard_hash: Arc::new(AtomicU64::new(0)),
        projection_viewer,
        projection_cancel: Arc::new(AtomicBool::new(false)),
        projection_generation: Arc::new(AtomicU64::new(0)),
        file_transfer_enabled: Arc::new(AtomicBool::new(true)),
        drag_badge: drag_badge.clone(),
        mobile_server,
    };

    let discovered_for_disc = state.discovered_devices.clone();
    let peer_device_for_disc = state.peer_device.clone();
    let discovered_for_rx = state.discovered_devices.clone();
    let peer_device_for_rx = state.peer_device.clone();
    let live_rtt_for_rx = state.live_rtt_us.clone();
    let paired_ip_for_rx = state.paired_peer_ip.clone();
    let keyboard_sharing_for_rx = state.keyboard_sharing.clone();
    let drag_badge_for_rx = drag_badge.clone();
    let app_handle_for_drop = Arc::new(Mutex::new(None::<tauri::AppHandle>));
    let app_handle_for_setup = app_handle_for_drop.clone();

    tauri::async_runtime::spawn(async move {
        let my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Desktop-PC".into());
        let (sw, sh) = get_screen_dimensions();
        let beacon = DiscoveryBeacon::new(my_name, DeviceRole::Host, 48100, 48101, sw, sh);
        if let Ok(emitter) = DiscoveryBeaconEmitter::new(beacon).await {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            loop {
                ticker.tick().await;
                let _ = emitter.emit_beacon().await;
            }
        }
    });

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
                    let peer_ip = peer.ip().to_string();

                    if let Ok(mut ip_guard) = paired_ip_for_rx.lock() {
                        if ip_guard.is_none() {
                            *ip_guard = Some(peer_ip.clone());
                        }
                    }

                    if let InputEvent::Heartbeat { ref device_name } = event {
                        if !device_name.is_empty() {
                            if let Ok(mut cur) = peer_device_for_rx.lock() {
                                *cur = Some(device_name.clone());
                            }
                            if let Ok(mut guard) = discovered_for_rx.lock() {
                                if let Some(d) = guard.get_mut(&peer_ip) {
                                    d.name = device_name.clone();
                                }
                            }
                        }
                    }

                    if let InputEvent::Keyboard { .. } = event {
                        if !keyboard_sharing_for_rx.load(Ordering::Relaxed) {
                            continue;
                        }
                    }

                    if let InputEvent::FileDrag { active, ref file_name } = event {
                        if active {
                            drag_badge_for_rx.show(0, 0, file_name);
                        } else {
                            drag_badge_for_rx.hide();
                            net::clear_file_drag();
                        }
                        continue;
                    }

                    if let InputEvent::CursorPosition { x, y } = event {
                        if drag_badge_for_rx.is_active() {
                            drag_badge_for_rx.update_position(x, y);
                        }
                    }

                    if let InputEvent::MouseMove { .. } = event {
                        if drag_badge_for_rx.is_active() {
                            unsafe {
                                let mut pt = windows::Win32::Foundation::POINT::default();
                                if windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt).is_ok() {
                                    drag_badge_for_rx.update_position(pt.x, pt.y);
                                }
                            }
                        }
                    }

                    if let InputEvent::MouseButton {
                        button: common::MouseButton::Left,
                        state: common::ButtonState::Released,
                    } = event
                    {
                        if drag_badge_for_rx.is_active() {
                            drag_badge_for_rx.hide();
                            let target_dir = net::get_target_dir_at_cursor();
                            let moved = net::commit_file_drop(target_dir);
                            if !moved.is_empty() {
                                if let Ok(guard) = app_handle_for_drop.lock() {
                                    if let Some(ref handle) = *guard {
                                        use tauri::Emitter;
                                        let names: Vec<String> = moved
                                            .iter()
                                            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
                                            .collect();
                                        let _ = handle.emit("file-received", names);
                                    }
                                }
                            }
                        }
                    }

                    let now_us = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64;
                    let latency_us = now_us.saturating_sub(header.timestamp_us);
                    live_rtt_for_rx.store(latency_us, Ordering::Relaxed);

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
                        if peer_name != peer_ip {
                            *cur = Some(peer_name);
                        } else if cur.is_none() {
                            *cur = Some(peer_ip.clone());
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

            let incoming_ip = peer_addr.ip().to_string();
            if let Ok(mut guard) = paired_ip_for_srv.lock() {
                if guard.is_none() {
                    *guard = Some(incoming_ip);
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
                            if !key.is_empty() && !clip_packet.token.is_empty() && clip_packet.token != *key {
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

    std::thread::Builder::new()
        .name("nerve-clipboard-sender".into())
        .spawn(move || {
            while let Ok(payload) = clip_rx.recv() {
                if !clipboard_sharing_for_tx.load(Ordering::Relaxed) {
                    continue;
                }

                let target_ip = if let Ok(guard) = paired_ip_for_tx.lock() {
                    guard.clone()
                } else {
                    None
                };

                if let Some(ip) = target_ip {
                    let token = if let Ok(guard) = paired_key_for_tx.lock() {
                        guard.clone().unwrap_or_default()
                    } else {
                        String::new()
                    };

                    match payload {
                        clipboard::ClipboardPayload::Text(t) => {
                            let packet = ClipboardPacket { token, text: t };
                            if let Ok(bytes) = serde_json::to_vec(&packet) {
                                let len = (bytes.len() as u32).to_be_bytes();
                                let target_addr = format!("{}:48102", ip);
                                if let Ok(mut stream) = std::net::TcpStream::connect_timeout(
                                    &target_addr.parse().unwrap(),
                                    Duration::from_millis(800),
                                ) {
                                    use std::io::Write;
                                    let _ = stream.write_all(&len);
                                    let _ = stream.write_all(&bytes);
                                    let _ = stream.flush();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        })
        .expect("Failed to spawn clipboard sender thread");

    let live_fps_for_vid_rx = state.live_fps_x10.clone();
    let viewer_for_vid_rx = state.projection_viewer.clone();
    tauri::async_runtime::spawn(async move {
        let bind_addr: SocketAddr = "0.0.0.0:48101".parse().unwrap();
        let mut video_receiver = match VideoReceiver::bind(bind_addr).await {
            Ok(r) => r,
            Err(_) => return,
        };

        let decoder_config = VideoDecoderConfig::default();
        let mut decoder = match HardwareVideoDecoder::new(decoder_config) {
            Ok(d) => d,
            Err(_) => return,
        };

        let mut frames = 0u64;
        let mut last_rep = Instant::now();

        loop {
            match video_receiver.recv_frame().await {
                Ok((frame_id, _is_keyframe, timestamp_us, frame_data)) => {
                    if let Ok(decoded) = decoder.decode_frame(frame_id, timestamp_us, &frame_data) {
                        if !decoded.bgra.is_empty() {
                            viewer_for_vid_rx.present_frame(decoded.width, decoded.height, 0, 0, decoded.bgra);
                        }
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
                live_fps_for_vid_rx.store((fps * 10.0) as u64, Ordering::Relaxed);
                frames = 0;
                last_rep = Instant::now();
            }
        }
    });

    let viewer_for_mirror = state.projection_viewer.clone();
    let fps_for_mirror = state.live_fps_x10.clone();
    tauri::async_runtime::spawn(async move {
        let receiver = match mirror_stream::MirrorReceiver::bind().await {
            Ok(receiver) => receiver,
            Err(e) => {
                error!("Mirror receiver cannot listen on TCP 48104: {e}");
                return;
            }
        };
        loop {
            let (stream, _peer_addr) = match receiver.accept().await {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("Mirror accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };

            let (mut read_half, mut write_half) = stream.into_split();
            let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

            let write_task = tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                while let Some(msg) = input_rx.recv().await {
                    if write_half.write_all(&msg).await.is_err() {
                        break;
                    }
                }
            });

            viewer_for_mirror.set_input_channel(Some(input_tx.clone()));
            input::disable_edge_detection();
            let mut frames = 0u64;
            let mut report = Instant::now();
            loop {
                let (width, height, offset_x, offset_y, jpeg) = match mirror_stream::MirrorReceiver::read_frame(&mut read_half).await {
                    Ok(frame) => frame,
                    Err(_) => break,
                };

                let _ = input_tx.send(vec![0x06]);
                frames += 1;

                let viewer = viewer_for_mirror.clone();
                tokio::task::spawn_blocking(move || {
                    match mirror_stream::decode_bgra(width, height, &jpeg) {
                        Ok(bgra) => viewer.present_frame(width, height, offset_x, offset_y, bgra),
                        Err(e) => warn!("Mirror JPEG decode failed: {e}"),
                    }
                });

                if report.elapsed() >= Duration::from_secs(1) {
                    fps_for_mirror.store((frames as f64 / report.elapsed().as_secs_f64() * 10.0) as u64, Ordering::Relaxed);
                    frames = 0;
                    report = Instant::now();
                }
            }
            viewer_for_mirror.set_input_channel(None);
            viewer_for_mirror.stop();
            write_task.abort();
            fps_for_mirror.store(0, Ordering::Relaxed);
            input::restore_edge_detection();
        }
    });

    if let Err(e) = tauri::Builder::default()
        .manage(state)
        .setup(move |app| {
            if let Ok(mut guard) = app_handle_for_setup.lock() {
                *guard = Some(app.handle().clone());
            }

            let file_key_ref = app.state::<AppState>().paired_security_key.clone();
            let file_enabled = app.state::<AppState>().file_transfer_enabled.clone();
            let file_app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tauri::Emitter;
                let cb = move |files: Vec<std::path::PathBuf>| {
                    let names: Vec<String> = files
                        .iter()
                        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
                        .collect();
                    let _ = file_app_handle.emit("file-received", names);
                };
                let _ = net::FileTransferServer::start(net::FILE_TRANSFER_PORT, file_key_ref, file_enabled, cb).await;
            });
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
                if std::env::args().any(|arg| arg == "--minimized") {
                    let _ = window.hide();
                }
            }

            let quit_i = MenuItem::with_id(app, "quit", "Вийти з NerveDesk", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "Відкрити вікно", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            let mut builder = TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("NerveDesk - KVM та віртуальний дисплей")
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
            stop_service,
            set_screen_edge,
            toggle_virtual_display,
            set_keyboard_sharing_enabled,
            set_clipboard_sharing_enabled,
            get_feature_settings,
            open_display_settings,
            install_virtual_display_driver,
            uninstall_virtual_display_driver,
            get_display_driver_status,
            get_display_driver_package_status,
            list_capture_windows,
            start_virtual_display_cast,
            stop_virtual_display_cast,
            open_received_folder,
            send_file_to_peer,
            set_file_transfer_enabled,
            get_autostart_enabled,
            set_autostart_enabled,
            get_mobile_remote_info,
            regenerate_mobile_pin,
            toggle_mobile_remote,
            open_mobile_remote_url
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
