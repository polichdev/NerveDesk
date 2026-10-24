use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{command, Manager, State};
use tokio::io::AsyncReadExt;
use tracing::{error, info, warn};

use capture::{
    elevate_driver, is_driver_installed, is_driver_package_available,
    list_windows as capture_list_windows, CaptureWindow, DxgiCaptureConfig,
};
use codec::{HardwareVideoEncoder, VideoEncoderConfig};
use common::InputEvent;
use engine::{
    get_screen_dimensions, mirror_worker, resolve_target_ip, stop_kvm_internal,
    stop_projection_internal, update_mode_string, AppMetrics, AppState, DiscoveredDevice,
    FeatureSettings, MobileRemoteInfo,
};
use input::InputHookManager;
use net::{
    generate_qr_svg, get_local_ip, AudioStatus, DeviceRole, DiscoveryBeacon,
    DiscoveryBeaconEmitter, FolderSync, MobileShareInfo, SyncPreview, SyncStatus, UdpInputSender,
    VideoSender,
};

#[command]
pub fn get_device_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Desktop-PC".into())
}

#[command]
pub fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[command]
pub fn show_main_window(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_minimized().unwrap_or(false) {
            let _ = window.unminimize();
        }
        let _ = window.show();
        let _ = window.set_focus();
        #[cfg(target_os = "windows")]
        crate::window::apply_windows_dark_theme_webview(&window);
    }
    if let Some(panel) = app.get_webview_window("tray-panel") {
        let _ = panel.hide();
    }
}

#[command]
pub fn get_paired_peer_ip(state: State<AppState>) -> Option<String> {
    state.paired_peer_ip.lock().ok().and_then(|ip| ip.clone())
}

#[command]
pub fn get_my_ip() -> String {
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
pub fn get_metrics(state: State<AppState>) -> AppMetrics {
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

    let paired_ip_for_display = state.paired_peer_ip.lock().unwrap().clone();
    if peer.is_none() {
        if let Some(ip) = paired_ip_for_display.as_ref() {
            peer = Some(
                state
                    .discovered_devices
                    .lock()
                    .ok()
                    .and_then(|devices| devices.get(ip).map(|device| device.name.clone()))
                    .unwrap_or_else(|| ip.clone()),
            );
        }
    }

    let edge = state.screen_edge.lock().unwrap().clone();
    let is_active =
        state.is_kvm_active.load(Ordering::SeqCst) || state.is_projecting.load(Ordering::SeqCst);
    let peer_ip = paired_ip_for_display;
    let peer_online = peer_ip.is_some()
        && state
            .authenticated_peer_seen
            .lock()
            .ok()
            .and_then(|seen| *seen)
            .is_some_and(|last_seen| last_seen.elapsed() < Duration::from_secs(5));
    let v_display = state.display_manager.is_plugged();

    let fps_x10 = if state.is_projecting.load(Ordering::SeqCst) {
        state.live_fps_x10.load(Ordering::Relaxed)
    } else {
        state
            .received_video_fps_x10
            .load(Ordering::Relaxed)
            .max(state.received_mirror_fps_x10.load(Ordering::Relaxed))
    };
    let fps = (fps_x10 as f64) / 10.0;
    let mbps = (state.live_mbps_x10.load(Ordering::Relaxed) as f64) / 10.0;

    AppMetrics {
        mode,
        is_active,
        kvm_active: state.is_kvm_active.load(Ordering::SeqCst),
        kvm_enabled: state.kvm_enabled.load(Ordering::SeqCst),
        mirror_enabled: state.mirror_enabled.load(Ordering::SeqCst),
        peer_online,
        file_transfer_enabled: state.file_transfer_enabled.load(Ordering::SeqCst),
        mobile_active: state.mobile_server.is_active(),
        audio_enabled: state.audio_bridge.status().enabled,
        audio_sending: state.audio_bridge.status().sending,
        audio_receiving: state.audio_bridge.status().receiving,
        folder_sync_enabled: state.folder_sync.status().enabled,
        video_fps: fps,
        bandwidth_mbps: mbps,
        peer_device: peer,
        virtual_display_active: v_display,
        projection_active: state.is_projecting.load(Ordering::SeqCst),
        screen_edge: edge,
    }
}

#[command]
pub async fn discover_devices(state: State<'_, AppState>) -> Result<Vec<DiscoveredDevice>, String> {
    let my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Host-PC".into());
    let (sw, sh) = get_screen_dimensions();
    let beacon = DiscoveryBeacon::new(my_name, DeviceRole::Host, 48100, 48101, sw, sh)
        .with_edge(state.screen_edge.lock().unwrap().clone());

    if let Ok(emitter) = DiscoveryBeaconEmitter::new(beacon).await {
        let _ = emitter.emit_beacon().await;
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    let devices: Vec<DiscoveredDevice> = if let (Ok(guard), Ok(seen)) = (
        state.discovered_devices.lock(),
        state.device_last_seen.lock(),
    ) {
        guard
            .values()
            .filter(|device| {
                seen.get(&device.ip)
                    .is_some_and(|last_seen| last_seen.elapsed() < Duration::from_secs(5))
            })
            .cloned()
            .collect()
    } else {
        Vec::new()
    };

    Ok(devices)
}

#[command]
pub async fn pair_device(
    target_name: String,
    security_key: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let clean_name = target_name.trim();
    if clean_name.is_empty() {
        return Err("Device name cannot be empty".into());
    }
    if !(16..=256).contains(&security_key.trim().len()) {
        return Err("Security key must contain 16 to 256 characters".into());
    }

    let my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Desktop-PC".into());
    let (sw, sh) = get_screen_dimensions();
    let beacon = DiscoveryBeacon::new(my_name, DeviceRole::Host, 48100, 48101, sw, sh)
        .with_edge(state.screen_edge.lock().unwrap().clone());
    if let Ok(emitter) = DiscoveryBeaconEmitter::new(beacon).await {
        let _ = emitter.emit_beacon().await;
    }

    let resolved_ip = resolve_target_ip(clean_name, &state).await?;
    crate::secret_store::save_key(security_key.trim())?;
    *state
        .authenticated_peer_seen
        .lock()
        .map_err(|e| e.to_string())? = None;
    if let Ok(mut key_guard) = state.paired_security_key.lock() {
        *key_guard = Some(security_key.trim().to_string());
    }
    if let Ok(mut ip_guard) = state.paired_peer_ip.lock() {
        *ip_guard = Some(resolved_ip);
    }
    if let Ok(mut peer_guard) = state.peer_device.lock() {
        *peer_guard = Some(clean_name.to_string());
    }
    Ok(format!("Device configured: {clean_name}"))
}

#[command]
pub fn get_saved_pairing_key() -> Result<Option<String>, String> {
    crate::secret_store::load_key()
}

#[command]
pub fn prepare_pairing_key(security_key: String, state: State<'_, AppState>) -> Result<(), String> {
    let key = security_key.trim();
    if !(16..=256).contains(&key.len()) {
        return Err("Security key must contain 16 to 256 characters".into());
    }
    crate::secret_store::save_key(key)?;
    let mut current = state
        .paired_security_key
        .lock()
        .map_err(|e| e.to_string())?;
    if current.as_deref() != Some(key) {
        *current = Some(key.to_string());
        *state.paired_peer_ip.lock().map_err(|e| e.to_string())? = None;
        *state
            .authenticated_peer_seen
            .lock()
            .map_err(|e| e.to_string())? = None;
    }
    Ok(())
}

#[command]
pub fn clear_pairing_key(state: State<'_, AppState>) -> Result<(), String> {
    stop_kvm_internal(&state);
    stop_projection_internal(&state);
    state.audio_bridge.set_enabled(false)?;
    crate::secret_store::clear_key()?;
    *state
        .paired_security_key
        .lock()
        .map_err(|e| e.to_string())? = None;
    *state.paired_peer_ip.lock().map_err(|e| e.to_string())? = None;
    *state
        .authenticated_peer_seen
        .lock()
        .map_err(|e| e.to_string())? = None;
    *state.peer_device.lock().map_err(|e| e.to_string())? = None;
    Ok(())
}

#[command]
pub async fn start_host(
    target_ip: String,
    stream_video: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    stop_kvm_internal(&state);

    let resolved_ip = resolve_target_ip(&target_ip, &state).await?;

    let peer_available = state
        .authenticated_peer_seen
        .lock()
        .ok()
        .and_then(|seen| *seen)
        .is_some_and(|last_seen| last_seen.elapsed() < Duration::from_secs(5));
    input::set_peer_available(peer_available);

    if let Ok(mut ip_guard) = state.paired_peer_ip.lock() {
        *ip_guard = Some(resolved_ip.clone());
    }

    let target_addr: SocketAddr = format!("{resolved_ip}:48100")
        .parse()
        .map_err(|e| format!("Invalid target address: {e}"))?;

    info!("Starting internal Host KVM engine -> input={}", target_addr);

    let security_key = state
        .paired_security_key
        .lock()
        .ok()
        .and_then(|key| key.clone())
        .ok_or_else(|| "Pair this device with a security key first".to_string())?;
    let input_sender = UdpInputSender::new(target_addr, security_key)
        .await
        .map_err(|e| format!("Failed to create UDP input sender: {e}"))?
        .with_peer_ip_tracker(state.paired_peer_ip.clone());

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
    let mut peer_edge = input::opposite_edge(screen_edge);
    if let Ok(guard) = state.discovered_devices.lock() {
        if let Some(dev) = guard.get(&target_addr.ip().to_string()) {
            client_w = dev.screen_width as i32;
            client_h = dev.screen_height as i32;
            peer_edge = parse_screen_edge(&dev.screen_edge).unwrap_or(peer_edge);
        }
    }
    input::set_active_edges_and_resolution(screen_edge, peer_edge, client_w, client_h);

    let is_running_in = state.is_kvm_active.clone();
    is_running_in.store(true, Ordering::SeqCst);
    let online_running = is_running_in.clone();
    let online_seen = state.authenticated_peer_seen.clone();
    let online_worker = tokio::spawn(async move {
        while online_running.load(Ordering::SeqCst) {
            let available = online_seen
                .lock()
                .ok()
                .and_then(|seen| *seen)
                .is_some_and(|last_seen| last_seen.elapsed() < Duration::from_secs(5));
            input::set_peer_available(available);
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    });
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
    let drag_badge_for_host = state.drag_badge.clone();
    let paired_ip_for_files = state.paired_peer_ip.clone();

    input::set_drag_resolver(|source_win| {
        let files = net::get_dragged_files(source_win);
        let first_name = files
            .first()
            .and_then(|f| f.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        (first_name, files)
    });

    let input_worker = tokio::spawn(async move {
        let mut last_hb = Instant::now();
        while is_running_in.load(Ordering::SeqCst) {
            match event_rx.try_recv() {
                Ok(event) => {
                    if let InputEvent::FileDrag { active, file_name } = event {
                        if active && file_name == "request" {
                            let mut pt = windows::Win32::Foundation::POINT::default();
                            unsafe {
                                let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
                            }
                            drag_badge_for_host.show(pt.x, pt.y, "File");
                            let _ = input_sender
                                .send_event(&InputEvent::FileDrag {
                                    active: true,
                                    file_name,
                                })
                                .await;
                        } else if active && is_file_transfer_enabled.load(Ordering::Relaxed) {
                            let dragged_files = input::take_cached_drag_files();
                            if !dragged_files.is_empty() {
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
                                let addr = paired_ip_for_files
                                    .lock()
                                    .ok()
                                    .and_then(|ip| ip.clone())
                                    .map(|ip| format!("{ip}:{}", net::FILE_TRANSFER_PORT))
                                    .unwrap_or_else(|| format!("{}:{}", target_addr.ip(), net::FILE_TRANSFER_PORT));
                                tokio::spawn(async move {
                                    if let Err(e) = net::FileTransferClient::send_files(
                                        &addr,
                                        &token,
                                        &dragged_files,
                                    )
                                    .await
                                    {
                                        warn!("Dragged file transfer failed: {e}");
                                    }
                                });
                            } else {
                                let _ = input_sender
                                    .send_event(&InputEvent::FileDrag {
                                        active: false,
                                        file_name: "cancel".into(),
                                    })
                                    .await;
                            }
                        } else {
                            drag_badge_for_host.hide();
                            if !active && file_name == "commit" {
                                let target_dir = net::get_target_dir_at_cursor();
                                let _ = net::commit_file_drop(target_dir);
                            }
                            let _ = input_sender
                                .send_event(&InputEvent::FileDrag {
                                    active: false,
                                    file_name,
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
    *state.kvm_workers.lock().unwrap() = vec![input_worker, online_worker];

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
    state.kvm_enabled.store(true, Ordering::SeqCst);

    if stream_video {
        let _ = start_virtual_display_cast(target_ip.clone(), false, None, state.clone()).await;
    }

    update_mode_string(&state);
    Ok(format!("Host service active -> {}", target_addr))
}

#[command]
pub fn list_capture_windows() -> Vec<CaptureWindow> {
    capture_list_windows()
}

#[command]
pub async fn start_virtual_display_cast(
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
        Some(
            tokio::net::TcpListener::bind("127.0.0.1:48103")
                .await
                .map_err(|e| format!("Cannot listen for NerveDesk display frames: {e}"))?,
        )
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
            if state.display_manager.is_plugged() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        state
            .display_manager
            .plug_display()
            .map_err(|e| e.to_string())?;
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

    let video_key = state
        .paired_security_key
        .lock()
        .ok()
        .and_then(|key| key.clone());
    let vid_worker = if let Some(listener) = frame_listener {
        let video_key =
            video_key.ok_or_else(|| "Pair this device before streaming its display".to_string())?;
        tokio::spawn(async move {
            let sender = match VideoSender::new(video_target, video_key).await {
                Ok(sender) => sender,
                Err(e) => {
                    error!("VideoSender error: {:?}", e);
                    return;
                }
            };
            let encoder = match HardwareVideoEncoder::new(VideoEncoderConfig::default()) {
                Ok(encoder) => encoder,
                Err(e) => {
                    error!("Encoder error: {:?}", e);
                    return;
                }
            };
            let mut frame_count = 0u64;
            let mut byte_count = 0u64;
            let mut report = Instant::now();
            while !cancel_flag.load(Ordering::Relaxed) && is_running_vid.load(Ordering::SeqCst) {
                let (mut stream, _) =
                    match tokio::time::timeout(Duration::from_millis(250), listener.accept()).await
                    {
                        Ok(Ok(connection)) => connection,
                        Ok(Err(e)) => {
                            warn!("IDD frame connection failed: {:?}", e);
                            continue;
                        }
                        Err(_) => continue,
                    };
                loop {
                    if cancel_flag.load(Ordering::Relaxed) || !is_running_vid.load(Ordering::SeqCst)
                    {
                        break;
                    }
                    let mut header = [0u8; 16];
                    if !matches!(
                        tokio::time::timeout(
                            Duration::from_secs(2),
                            stream.read_exact(&mut header)
                        )
                        .await,
                        Ok(Ok(_))
                    ) {
                        break;
                    }
                    let magic = u32::from_le_bytes(header[0..4].try_into().unwrap());
                    let width = u32::from_le_bytes(header[4..8].try_into().unwrap());
                    let height = u32::from_le_bytes(header[8..12].try_into().unwrap());
                    let len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
                    if magic != 0x3146444e
                        || width == 0
                        || height == 0
                        || width > 7680
                        || height > 4320
                        || len != (width as usize) * (height as usize) * 4
                    {
                        warn!("Invalid IDD frame header");
                        break;
                    }
                    let mut pixels = vec![0u8; len];
                    if !matches!(
                        tokio::time::timeout(
                            Duration::from_secs(2),
                            stream.read_exact(&mut pixels)
                        )
                        .await,
                        Ok(Ok(_))
                    ) {
                        break;
                    }
                    let now_us = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64;
                    let keyframe = frame_count == 0 || frame_count % 60 == 0;
                    if let Ok(frame) = encoder.encode_bgra(width, height, &pixels, now_us, keyframe)
                    {
                        if let Ok(bytes) = sender.send_frame(&frame).await {
                            frame_count += 1;
                            byte_count += bytes as u64;
                        }
                    }
                    if report.elapsed() >= Duration::from_secs(1) {
                        let elapsed = report.elapsed().as_secs_f64();
                        fps_holder.store(
                            (frame_count as f64 / elapsed * 10.0) as u64,
                            Ordering::Relaxed,
                        );
                        mbps_holder.store(
                            (byte_count as f64 * 8.0 / elapsed / 100_000.0) as u64,
                            Ordering::Relaxed,
                        );
                        frame_count = 0;
                        byte_count = 0;
                        report = Instant::now();
                    }
                }
            }
        })
    } else {
        let mirror_target: SocketAddr = format!("{}:48104", resolved_ip)
            .parse()
            .map_err(|e| format!("Invalid mirror target address: {e}"))?;
        let security_key = state
            .paired_security_key
            .lock()
            .ok()
            .and_then(|key| key.clone())
            .ok_or_else(|| "Pair this device before streaming its screen".to_string())?;
        match mirror_worker::start(
            mirror_target,
            security_key,
            source_window,
            mirror_capture_config,
            cancel_flag,
            is_running_vid,
            fps_holder,
            mbps_holder,
            generation,
            current_generation,
        )
        .await
        {
            Ok(worker) => worker,
            Err(e) => {
                state.is_projecting.store(false, Ordering::SeqCst);
                return Err(e);
            }
        }
    };

    *state.projection_workers.lock().unwrap() = vec![vid_worker];
    state.mirror_enabled.store(true, Ordering::SeqCst);
    update_mode_string(&state);

    if extend_mode {
        Ok(format!("Projection active -> {}", video_target))
    } else {
        Ok(format!("Projection active -> {}", resolved_ip))
    }
}

#[command]
pub fn stop_virtual_display_cast(state: State<'_, AppState>) {
    stop_projection_internal(&state);
}

#[command]
pub fn stop_service(state: State<'_, AppState>) -> Result<String, String> {
    stop_kvm_internal(&state);
    Ok("KVM stopped safely".into())
}

#[command]
pub fn set_module_enabled(
    module: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    match module.as_str() {
        "kvm" => {
            state.kvm_enabled.store(enabled, Ordering::SeqCst);
            if !enabled && state.is_kvm_active.load(Ordering::SeqCst) {
                stop_kvm_internal(&state);
            }
        }
        "mirror" => {
            state.mirror_enabled.store(enabled, Ordering::SeqCst);
            if !enabled && state.is_projecting.load(Ordering::SeqCst) {
                stop_projection_internal(&state);
            }
        }
        _ => return Err(format!("Unknown module: {module}")),
    }
    Ok(enabled)
}

#[command]
pub fn set_screen_edge(edge: String, state: State<'_, AppState>) -> Result<String, String> {
    let screen_edge = match edge.as_str() {
        "Left" => input::ScreenEdge::Left,
        "Right" => input::ScreenEdge::Right,
        "Top" => input::ScreenEdge::Top,
        "Bottom" => input::ScreenEdge::Bottom,
        _ => return Err("Invalid edge value. Use Left, Right, Top, or Bottom".into()),
    };

    let (mut client_w, mut client_h) = (0, 0);
    let mut peer_edge = input::opposite_edge(screen_edge);
    if let Ok(guard) = state.discovered_devices.lock() {
        if let Ok(peer_guard) = state.paired_peer_ip.lock() {
            if let Some(dev) = peer_guard.as_ref().and_then(|ip| guard.get(ip)) {
                client_w = dev.screen_width as i32;
                client_h = dev.screen_height as i32;
                peer_edge = parse_screen_edge(&dev.screen_edge).unwrap_or(peer_edge);
            }
        }
    }

    input::set_active_edges_and_resolution(screen_edge, peer_edge, client_w, client_h);
    *state.screen_edge.lock().unwrap() = edge.clone();
    info!("Active crossing boundary adjusted to: {}", edge);
    Ok(format!("Screen edge set to {}", edge))
}

fn parse_screen_edge(edge: &str) -> Option<input::ScreenEdge> {
    match edge {
        "Left" => Some(input::ScreenEdge::Left),
        "Right" => Some(input::ScreenEdge::Right),
        "Top" => Some(input::ScreenEdge::Top),
        "Bottom" => Some(input::ScreenEdge::Bottom),
        _ => None,
    }
}

#[command]
pub fn toggle_virtual_display(state: State<'_, AppState>) -> Result<bool, String> {
    let _ = state;
    Err("Use Windows Display Settings to enable or disable the NerveDesk display".into())
}

#[command]
pub fn set_keyboard_sharing_enabled(enabled: bool, state: State<'_, AppState>) {
    state.keyboard_sharing.store(enabled, Ordering::SeqCst);
    input::set_keyboard_sharing(enabled);
}

#[command]
pub fn set_clipboard_sharing_enabled(enabled: bool, state: State<'_, AppState>) {
    state.clipboard_sharing.store(enabled, Ordering::SeqCst);
}

#[command]
pub fn get_feature_settings(state: State<'_, AppState>) -> FeatureSettings {
    FeatureSettings {
        keyboard_sharing: state.keyboard_sharing.load(Ordering::SeqCst),
        clipboard_sharing: state.clipboard_sharing.load(Ordering::SeqCst),
    }
}

#[command]
pub fn open_display_settings() {
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "ms-settings:display"])
        .spawn();
}

#[command]
pub fn install_virtual_display_driver() -> Result<String, String> {
    elevate_driver("install")
}

#[command]
pub fn uninstall_virtual_display_driver() -> Result<String, String> {
    elevate_driver("remove")
}

#[command]
pub fn get_display_driver_status() -> bool {
    is_driver_installed()
}

#[command]
pub fn get_display_driver_package_status() -> bool {
    is_driver_package_available()
}

#[command]
pub async fn open_received_folder(path: Option<String>) -> Result<(), String> {
    let dir = path
        .as_deref()
        .map(std::path::Path::new)
        .and_then(|p| {
            if p.is_file() {
                p.parent()
            } else if p.is_dir() {
                Some(p)
            } else {
                None
            }
        })
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(net::get_default_transfer_dir);
    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
    Ok(())
}

#[command]
pub fn set_file_transfer_enabled(enabled: bool, state: State<'_, AppState>) {
    state.file_transfer_enabled.store(enabled, Ordering::SeqCst);
}

#[command]
pub fn get_autostart_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        if let Ok(output) = std::process::Command::new("reg")
            .args([
                "query",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "NerveDesk",
            ])
            .output()
        {
            return output.status.success();
        }
    }
    false
}

#[command]
pub fn set_autostart_enabled(enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        if enabled {
            let exe_path = std::env::current_exe().map_err(|e| e.to_string())?;
            let exe_str = exe_path
                .to_str()
                .ok_or_else(|| "Invalid exe path".to_string())?;
            let val = format!("\"{}\" --minimized", exe_str);
            let status = std::process::Command::new("reg")
                .args([
                    "add",
                    "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                    "/v",
                    "NerveDesk",
                    "/t",
                    "REG_SZ",
                    "/d",
                    &val,
                    "/f",
                ])
                .status()
                .map_err(|e| e.to_string())?;
            if !status.success() {
                return Err("Failed to register autostart in Windows Registry".into());
            }
        } else {
            let _ = std::process::Command::new("reg")
                .args([
                    "delete",
                    "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                    "/v",
                    "NerveDesk",
                    "/f",
                ])
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
pub async fn send_file_to_peer(path: String, state: State<'_, AppState>) -> Result<String, String> {
    if !state.file_transfer_enabled.load(Ordering::SeqCst) {
        return Err("File transfer is disabled".into());
    }
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
pub fn get_mobile_remote_info(state: State<'_, AppState>) -> MobileRemoteInfo {
    let ip = get_local_ip();
    let port = state.mobile_server.port();
    let is_active = state.mobile_server.is_active();
    let url = format!("https://{}:{}", ip, port);
    let qr_svg = generate_qr_svg(&url).unwrap_or_default();
    let pin = state.mobile_server.get_pin();
    let certificate_fingerprint = state
        .mobile_server
        .certificate_fingerprint()
        .unwrap_or_default();
    MobileRemoteInfo {
        is_active,
        ip,
        port,
        url,
        qr_svg,
        pin,
        certificate_fingerprint,
    }
}

#[command]
pub fn regenerate_mobile_pin(state: State<'_, AppState>) -> MobileRemoteInfo {
    state.mobile_server.regenerate_pin();
    get_mobile_remote_info(state)
}

#[command]
pub fn toggle_mobile_remote(enable: bool, state: State<'_, AppState>) -> bool {
    if enable {
        state.mobile_server.start();
    } else {
        state.mobile_server.stop();
    }
    state.mobile_server.is_active()
}

#[command]
pub fn set_mobile_audio_enabled(enabled: bool, state: State<'_, AppState>) -> bool {
    state.mobile_server.set_audio_enabled(enabled)
}

#[command]
pub fn get_mobile_share_info(state: State<'_, AppState>) -> MobileShareInfo {
    state.mobile_server.share_info()
}

#[command]
pub fn share_mobile_text(
    text: String,
    state: State<'_, AppState>,
) -> Result<MobileShareInfo, String> {
    state.mobile_server.share_text(text)
}

#[command]
pub fn clear_mobile_share(state: State<'_, AppState>) -> MobileShareInfo {
    state.mobile_server.clear_share()
}

#[command]
pub fn pick_mobile_share_file(
    state: State<'_, AppState>,
) -> Result<Option<MobileShareInfo>, String> {
    let selected = std::thread::spawn(|| unsafe {
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
            COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
        let result: Result<Option<String>, String> = (|| {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            if dialog.Show(None).is_err() {
                return Ok(None);
            }
            let item = dialog.GetResult().map_err(|e| e.to_string())?;
            let name = item
                .GetDisplayName(SIGDN_FILESYSPATH)
                .map_err(|e| e.to_string())?;
            let path = name.to_string().map_err(|e| e.to_string())?;
            CoTaskMemFree(Some(name.0.cast()));
            Ok(Some(path))
        })();
        CoUninitialize();
        result
    })
    .join()
    .map_err(|_| "File picker failed".to_string())??;
    selected
        .map(|path| state.mobile_server.share_file(Path::new(&path)))
        .transpose()
}

#[command]
pub fn get_mobile_receive_folder(state: State<'_, AppState>) -> String {
    state.mobile_server.receive_dir()
}

#[command]
pub fn set_mobile_receive_folder(
    path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    state.mobile_server.set_receive_dir(Path::new(&path))
}

#[command]
pub fn reset_mobile_receive_folder(state: State<'_, AppState>) -> Result<String, String> {
    state.mobile_server.reset_receive_dir()
}

#[command]
pub fn open_mobile_received_folder(state: State<'_, AppState>) -> Result<(), String> {
    let dir = std::path::PathBuf::from(state.mobile_server.receive_dir());
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer")
        .arg(dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[command]
pub fn open_mobile_remote_url(state: State<'_, AppState>) {
    let ip = get_local_ip();
    let port = state.mobile_server.port();
    let url = format!("https://{}:{}", ip, port);
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", &url])
        .spawn();
}

#[command]
pub fn get_audio_status(state: State<'_, AppState>) -> AudioStatus {
    state.audio_bridge.status()
}

#[command]
pub fn set_audio_enabled(state: State<'_, AppState>, enabled: bool) -> Result<AudioStatus, String> {
    state.audio_bridge.set_enabled(enabled)
}

#[command]
pub async fn start_audio_stream(
    state: State<'_, AppState>,
    target_ip: String,
) -> Result<AudioStatus, String> {
    let ip = resolve_target_ip(&target_ip, &state).await?;
    state.audio_bridge.start(ip)
}

#[command]
pub fn stop_audio_stream(state: State<'_, AppState>) -> AudioStatus {
    state.audio_bridge.stop()
}

#[command]
pub async fn request_audio_stream(
    state: State<'_, AppState>,
    target_ip: String,
) -> Result<AudioStatus, String> {
    let ip = resolve_target_ip(&target_ip, &state).await?;
    let bridge = state.audio_bridge.clone();
    tokio::task::spawn_blocking(move || bridge.request_from(ip))
        .await
        .map_err(|e| e.to_string())?
}

#[command]
pub async fn stop_requested_audio_stream(
    state: State<'_, AppState>,
) -> Result<AudioStatus, String> {
    let bridge = state.audio_bridge.clone();
    tokio::task::spawn_blocking(move || bridge.stop_requested())
        .await
        .map_err(|e| e.to_string())
}

#[command]
pub fn get_folder_sync_status(state: State<'_, AppState>) -> SyncStatus {
    state.folder_sync.status()
}

#[command]
pub fn generate_folder_sync_code() -> Result<String, String> {
    FolderSync::generate_code()
}

#[command]
pub fn set_folder_sync_enabled(
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<SyncStatus, String> {
    state.folder_sync.set_enabled(enabled)
}

#[command]
pub async fn add_folder_sync_pair(
    state: State<'_, AppState>,
    peer_ip: String,
    peer_name: String,
    local_dir: String,
) -> Result<SyncStatus, String> {
    let manager = state.folder_sync.clone();
    tokio::task::spawn_blocking(move || manager.add_pair(peer_ip, peer_name, local_dir))
        .await
        .map_err(|e| e.to_string())?
}

#[command]
pub fn remove_folder_sync_pair(
    state: State<'_, AppState>,
    id: String,
) -> Result<SyncStatus, String> {
    state.folder_sync.remove_pair(&id)
}

#[command]
pub async fn sync_folder_now(state: State<'_, AppState>, id: String) -> Result<SyncStatus, String> {
    let manager = state.folder_sync.clone();
    tokio::task::spawn_blocking(move || manager.sync_now(&id))
        .await
        .map_err(|e| e.to_string())?
}

#[command]
pub async fn preview_folder_sync(
    state: State<'_, AppState>,
    id: String,
) -> Result<SyncPreview, String> {
    let manager = state.folder_sync.clone();
    tokio::task::spawn_blocking(move || manager.preview(&id))
        .await
        .map_err(|e| e.to_string())?
}

#[command]
pub fn approve_folder_sync_pair(
    state: State<'_, AppState>,
    id: String,
) -> Result<SyncStatus, String> {
    state.folder_sync.approve(&id)
}

#[command]
pub fn pick_folder_sync_directory() -> Result<Option<String>, String> {
    std::thread::spawn(|| unsafe {
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
            COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::UI::Shell::{
            FileOpenDialog, IFileOpenDialog, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
        };
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(|e| e.to_string())?;
        let result = (|| {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            let options = dialog.GetOptions().map_err(|e| e.to_string())?;
            dialog
                .SetOptions(options | FOS_PICKFOLDERS)
                .map_err(|e| e.to_string())?;
            if dialog.Show(None).is_err() {
                return Ok(None);
            }
            let item = dialog.GetResult().map_err(|e| e.to_string())?;
            let name = item
                .GetDisplayName(SIGDN_FILESYSPATH)
                .map_err(|e| e.to_string())?;
            let path = name.to_string().map_err(|e| e.to_string())?;
            CoTaskMemFree(Some(name.0.cast()));
            Ok(Some(path))
        })();
        CoUninitialize();
        result
    })
    .join()
    .map_err(|_| "Folder picker failed".to_string())?
}

#[command]
pub async fn restore_folder_sync_version(
    state: State<'_, AppState>,
    pair_id: String,
    version_id: String,
) -> Result<SyncStatus, String> {
    let manager = state.folder_sync.clone();
    tokio::task::spawn_blocking(move || manager.restore_version(&pair_id, &version_id))
        .await
        .map_err(|e| e.to_string())?
}
