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
    let discovered_for_rx = discovered_cache.clone();

    let peer_device_arc = Arc::new(Mutex::new(None));
    let peer_device_for_rx = peer_device_arc.clone();

    let live_rtt_arc = Arc::new(AtomicU64::new(0));
    let live_rtt_for_rx = live_rtt_arc.clone();

    let state = AppState {
        is_running: Arc::new(AtomicBool::new(false)),
        mode: Mutex::new("Idle".into()),
        peer_device: peer_device_arc,
        screen_edge: Mutex::new("Right".into()),
        display_manager: VirtualDisplayManager::new(),
        hook_manager: Mutex::new(None),
        active_workers: Mutex::new(Vec::new()),
        live_rtt_us: live_rtt_arc,
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
                        if cur.as_deref() != Some(&peer_name) {
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
                            let text_color: u32 = 0x00f4f4f5;
                            let _ = DwmSetWindowAttribute(
                                hwnd,
                                DWMWINDOWATTRIBUTE(36),
                                &text_color as *const _ as _,
                                std::mem::size_of::<u32>() as u32,
                            );
                        }
                    }
                }
            }

            let quit_i = MenuItem::with_id(app, "quit", "Quit NerveDesk", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "Open NerveDesk", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            let mut builder = TrayIconBuilder::new()
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| {
                    match event.id.as_ref() {
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
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
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