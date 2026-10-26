use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tracing::{error, info, warn};

use codec::{HardwareVideoDecoder, VideoDecoderConfig};
use common::InputEvent;
use input::InputEmulator;
use net::{
    decode_mirror_bgra, DeviceRole, DiscoveryBeacon, DiscoveryBeaconEmitter, DiscoveryListener,
    MirrorCodec, MirrorReceiver, UdpInputReceiver, UdpInputSender, VideoReceiver,
};

use crate::state::{get_screen_dimensions, AppState, ClipboardPacket, DiscoveredDevice};

pub type FileReceivedCallback = Arc<dyn Fn(Vec<String>) + Send + Sync>;

pub fn start_background_services(state: &AppState, on_file_received: Option<FileReceivedCallback>) {
    spawn_discovery_emitter(state);
    spawn_discovery_listener(state);
    spawn_pair_probe(state);
    spawn_input_receiver(state, on_file_received);
    spawn_clipboard_receiver(state);
    spawn_clipboard_sender(state);
    spawn_video_receiver(state);
    spawn_mirror_receiver(state);
}

fn spawn_pair_probe(state: &AppState) {
    let paired_ip = state.paired_peer_ip.clone();
    let paired_key = state.paired_security_key.clone();
    let authenticated = state.authenticated_peer_seen.clone();
    let discovered_for_probe = state.discovered_devices.clone();
    let peer_device_for_probe = state.peer_device.clone();
    engine_async_spawn(async move {
        let socket = match tokio::net::UdpSocket::bind("0.0.0.0:0").await {
            Ok(socket) => socket,
            Err(e) => {
                warn!("Pair probe socket failed: {e}");
                return;
            }
        };
        let mut response = [0u8; 128];
        loop {
            let mut ip = paired_ip.lock().ok().and_then(|ip| ip.clone());
            if ip.is_none() {
                if let (Ok(disc_guard), Ok(peer_guard)) =
                    (discovered_for_probe.lock(), peer_device_for_probe.lock())
                {
                    if let Some(ref target_name) = *peer_guard {
                        if let Some((disc_ip, _)) = disc_guard
                            .iter()
                            .find(|(_, dev)| dev.name.eq_ignore_ascii_case(target_name))
                        {
                            ip = Some(disc_ip.clone());
                            if let Ok(mut paired_guard) = paired_ip.lock() {
                                if paired_guard.is_none() {
                                    *paired_guard = Some(disc_ip.clone());
                                }
                            }
                        }
                    }
                }
            }
            let key = paired_key.lock().ok().and_then(|key| key.clone());
            if let (Some(ip), Some(key)) = (ip, key) {
                if let Ok(address) = format!("{ip}:48100").parse::<SocketAddr>() {
                    if let Ok(packet) =
                        net::secure_input::seal_payload(&key, b"pair-probe", b"NerveDesk", 32)
                    {
                        let challenge = &packet[..12];
                        let _ = socket.send_to(&packet, address).await;
                        if let Ok(Ok((len, sender))) = tokio::time::timeout(
                            Duration::from_millis(500),
                            socket.recv_from(&mut response),
                        )
                        .await
                        {
                            if sender.ip() == address.ip()
                                && net::secure_input::ReplayGuard::new()
                                    .open_payload(&key, b"pair-ack", &response[..len], 32)
                                    .ok()
                                    .as_deref()
                                    == Some(challenge)
                            {
                                if let Ok(mut seen) = authenticated.lock() {
                                    *seen = Some(Instant::now());
                                }
                            }
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

fn spawn_discovery_emitter(state: &AppState) {
    let paired_ip_for_disc = state.paired_peer_ip.clone();
    let paired_key_for_disc = state.paired_security_key.clone();
    let edge_for_disc = state.screen_edge.clone();
    engine_async_spawn(async move {
        let my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Desktop-PC".into());
        let (sw, sh) = get_screen_dimensions();
        let beacon = DiscoveryBeacon::new(my_name, DeviceRole::Host, 48100, 48101, sw, sh);
        if let Ok(mut emitter) = DiscoveryBeaconEmitter::new(beacon).await {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            loop {
                ticker.tick().await;
                if let Ok(edge) = edge_for_disc.lock() {
                    emitter.set_screen_edge(edge.clone());
                }

                let current_key = paired_key_for_disc.lock().ok().and_then(|k| k.clone());
                emitter.set_pairing_key(current_key.clone());

                let _ = emitter.emit_beacon().await;

                let paired_ip = paired_ip_for_disc.lock().ok().and_then(|ip| ip.clone());
                if let Some(ip) = paired_ip {
                    if let Ok(address) = format!("{ip}:48103").parse() {
                        let _ = emitter.emit_to(address).await;
                    }
                }
            }
        }
    });
}

fn spawn_discovery_listener(state: &AppState) {
    let discovered_for_disc = state.discovered_devices.clone();
    let seen_for_disc = state.device_last_seen.clone();
    let peer_device_for_disc = state.peer_device.clone();
    let paired_ip_for_disc = state.paired_peer_ip.clone();
    let paired_key_for_disc = state.paired_security_key.clone();
    let edge_for_disc = state.screen_edge.clone();
    let authenticated_for_disc = state.authenticated_peer_seen.clone();

    engine_async_spawn(async move {
        if let Ok(listener) = DiscoveryListener::bind().await {
            loop {

                let current_key = paired_key_for_disc.lock().ok().and_then(|k| k.clone());
                let key_ref = current_key.as_deref();
                if let Ok((beacon, peer_addr, authenticated)) =
                    listener.recv_beacon_with_auth(key_ref).await
                {
                    let my_name = std::env::var("COMPUTERNAME").unwrap_or_default();
                    if !my_name.is_empty() && beacon.device_name.eq_ignore_ascii_case(&my_name) {
                        continue;
                    }
                    let ip = peer_addr.ip().to_string();
                    let mut is_paired = paired_ip_for_disc
                        .lock()
                        .ok()
                        .and_then(|p| p.clone())
                        .as_deref()
                        == Some(ip.as_str());

                    if !is_paired {
                        let matches_peer = peer_device_for_disc
                            .lock()
                            .ok()
                            .and_then(|p| p.clone())
                            .is_some_and(|name| name.eq_ignore_ascii_case(&beacon.device_name));
                        if matches_peer && authenticated {
                            if let Ok(mut guard) = paired_ip_for_disc.lock() {
                                *guard = Some(ip.clone());
                            }
                            is_paired = true;
                        }
                    }

                    if peer_addr.port() != 48103 {
                        let (sw, sh) = get_screen_dimensions();
                        let local_edge = edge_for_disc
                            .lock()
                            .map(|e| e.clone())
                            .unwrap_or_else(|_| "Right".into());
                        let reply =
                            DiscoveryBeacon::new(my_name, DeviceRole::Host, 48100, 48101, sw, sh)
                                .with_edge(local_edge);
                        let address = SocketAddr::new(peer_addr.ip(), 48103);

                        let reply_key = if is_paired { key_ref } else { None };
                        let _ = listener.reply_to(&reply, address, reply_key).await;
                    }
                    if is_paired && authenticated {
                        if let Some(edge) = edge_from_name(&beacon.screen_edge) {
                            input::set_remote_edge(edge);
                        }
                        if let Ok(mut auth) = authenticated_for_disc.lock() {
                            *auth = Some(Instant::now());
                        }
                    }
                    if let Ok(mut seen) = seen_for_disc.lock() {
                        seen.insert(ip.clone(), Instant::now());
                    }
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
                            existing.screen_edge = beacon.screen_edge.clone();
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
                                    screen_edge: beacon.screen_edge.clone(),
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
}

fn edge_from_name(name: &str) -> Option<input::ScreenEdge> {
    match name {
        "Left" => Some(input::ScreenEdge::Left),
        "Right" => Some(input::ScreenEdge::Right),
        "Top" => Some(input::ScreenEdge::Top),
        "Bottom" => Some(input::ScreenEdge::Bottom),
        _ => None,
    }
}

fn spawn_input_receiver(state: &AppState, on_file_received: Option<FileReceivedCallback>) {
    let discovered_for_rx = state.discovered_devices.clone();
    let seen_for_rx = state.device_last_seen.clone();
    let peer_device_for_rx = state.peer_device.clone();
    let paired_ip_for_rx = state.paired_peer_ip.clone();
    let state_key_for_input = state.paired_security_key.clone();
    let keyboard_sharing_for_rx = state.keyboard_sharing.clone();
    let drag_badge_for_rx = state.drag_badge.clone();
    let authenticated_for_rx = state.authenticated_peer_seen.clone();

    engine_async_spawn(async move {
        let bind_addr: SocketAddr = "0.0.0.0:48100".parse().unwrap();
        let mut receiver =
            match UdpInputReceiver::bind(bind_addr, state_key_for_input.clone(), paired_ip_for_rx.clone())
                .await
            {
                Ok(r) => r.with_authenticated_tracker(authenticated_for_rx.clone()),
                Err(e) => {
                    warn!("Auto background receiver bind failed: {:?}", e);
                    return;
                }
            };

        let emulator = InputEmulator::new();
        loop {
            match receiver.recv_event().await {
                Ok((_header, event, peer)) => {
                    let peer_ip = peer.ip().to_string();
                    let paired = paired_ip_for_rx.lock().ok().and_then(|ip| ip.clone());
                    if paired.as_deref() != Some(peer_ip.as_str()) {
                        continue;
                    }
                    if let Ok(mut seen) = seen_for_rx.lock() {
                        seen.insert(peer_ip.clone(), Instant::now());
                    }
                    if let Ok(mut auth) = authenticated_for_rx.lock() {
                        *auth = Some(Instant::now());
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

                    if let InputEvent::FileDrag {
                        active,
                        ref file_name,
                    } = event
                    {
                        if active {
                            if file_name == "request" {
                                let files = net::get_dragged_files(0);
                                if !files.is_empty() {
                                    let dragged_name = files[0]
                                        .file_name()
                                        .and_then(|name| name.to_str())
                                        .unwrap_or("File")
                                        .to_string();
                                    let token = state_key_for_input
                                        .lock()
                                        .ok()
                                        .and_then(|k| k.clone())
                                        .unwrap_or_default();
                                    let host_addr = format!("{peer_ip}:{}", net::FILE_TRANSFER_PORT);
                                    let input_addr = format!("{peer_ip}:48100").parse::<SocketAddr>();
                                    engine_async_spawn(async move {
                                        if let Ok(input_addr) = input_addr {
                                            if let Ok(sender) =
                                                UdpInputSender::new(input_addr, token.clone()).await
                                            {
                                                let _ = sender
                                                    .send_event(&InputEvent::FileDrag {
                                                        active: true,
                                                        file_name: dragged_name,
                                                    })
                                                    .await;
                                            }
                                        }
                                        let _ = net::FileTransferClient::send_files(
                                            &host_addr,
                                            &token,
                                            &files,
                                        )
                                        .await;
                                    });
                                }
                            } else {
                                let mut pt = windows::Win32::Foundation::POINT::default();
                                unsafe {
                                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
                                }
                                drag_badge_for_rx.show(pt.x, pt.y, file_name);
                            }
                        } else {
                            drag_badge_for_rx.hide();
                            if file_name == "cancel" {
                                net::clear_file_drag();
                            }
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
                                if windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt)
                                    .is_ok()
                                {
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
                            info!("File drop target: {}", target_dir.display());
                            let moved = net::commit_file_drop(target_dir);
                            if !moved.is_empty() {
                                if let Some(ref cb) = on_file_received {
                                    cb(moved
                                        .iter()
                                        .map(|p| p.to_string_lossy().into_owned())
                                        .collect());
                                }
                            }
                        }
                    }

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
}

fn spawn_clipboard_receiver(state: &AppState) {
    let clipboard_sharing_for_srv = state.clipboard_sharing.clone();
    let paired_key_for_srv = state.paired_security_key.clone();
    let paired_ip_for_srv = state.paired_peer_ip.clone();
    let clipboard_hash_for_srv = state.clipboard_hash.clone();
    let replay_guard = Arc::new(std::sync::Mutex::new(net::secure_input::ReplayGuard::new()));

    engine_async_spawn(async move {
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
            let paired = paired_ip_for_srv.lock().ok().and_then(|ip| ip.clone());
            if paired.as_deref() != Some(incoming_ip.as_str()) {
                continue;
            }

            let hash_ref = clipboard_hash_for_srv.clone();
            let key_ref = paired_key_for_srv.clone();
            let clipboard_enabled = clipboard_sharing_for_srv.clone();
            let replay_guard = replay_guard.clone();

            tokio::spawn(async move {
                let mut len_buf = [0u8; 4];
                if socket.read_exact(&mut len_buf).await.is_err() {
                    return;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                if len == 0 || len > 64 * 1024 * 1024 + 1052 {
                    return;
                }
                let mut encrypted = vec![0u8; len];
                if socket.read_exact(&mut encrypted).await.is_err() {
                    return;
                }
                let key = key_ref.lock().ok().and_then(|key| key.clone());
                let Some(key) = key else { return };
                let payload_buf = match replay_guard.lock() {
                    Ok(mut guard) => {
                        guard.open_payload(&key, b"clipboard", &encrypted, 64 * 1024 * 1024 + 1024)
                    }
                    Err(_) => return,
                };
                let Ok(mut payload_buf) = payload_buf else {
                    return;
                };

                let incoming = if payload_buf.starts_with(b"NCBI") && payload_buf.len() >= 10 {
                    let token_len = u16::from_be_bytes([payload_buf[4], payload_buf[5]]) as usize;
                    let offset = 6 + token_len;
                    if token_len > 256 || payload_buf.len() < offset + 4 {
                        None
                    } else {
                        let token = String::from_utf8_lossy(&payload_buf[6..offset]).into_owned();
                        let format =
                            u32::from_be_bytes(payload_buf[offset..offset + 4].try_into().unwrap());
                        let bytes = payload_buf.split_off(offset + 4);
                        Some((token, clipboard::ClipboardPayload::Image { format, bytes }))
                    }
                } else {
                    serde_json::from_slice::<ClipboardPacket>(&payload_buf)
                        .ok()
                        .map(|packet| {
                            (packet.token, clipboard::ClipboardPayload::Text(packet.text))
                        })
                };
                if let Some((token, payload)) = incoming {
                    if !clipboard_enabled.load(Ordering::SeqCst) {
                        return;
                    }
                    let valid_token = !key.is_empty() && token == key;
                    if !valid_token {
                        return;
                    }

                    let h = payload.compute_hash();
                    if hash_ref.swap(h, Ordering::SeqCst) != h {
                        let result = match payload {
                            clipboard::ClipboardPayload::Text(text) => {
                                clipboard::ClipboardWriter::write_text(&text)
                            }
                            clipboard::ClipboardPayload::Image { format, bytes } => {
                                clipboard::ClipboardWriter::write_image(format, &bytes)
                            }
                            clipboard::ClipboardPayload::Files(_) => return,
                        };
                        if let Err(e) = result {
                            warn!("Writing remote clipboard failed: {e}");
                        }
                    }
                }
            });
        }
    });
}

fn spawn_clipboard_sender(state: &AppState) {
    let (clip_tx, clip_rx) = crossbeam_channel::unbounded::<clipboard::ClipboardPayload>();
    let clipboard_hash_for_listener = state.clipboard_hash.clone();
    let clipboard_sharing_for_tx = state.clipboard_sharing.clone();
    let paired_key_for_tx = state.paired_security_key.clone();
    let paired_ip_for_tx = state.paired_peer_ip.clone();

    if let Ok(listener) = clipboard::ClipboardListener::start(clip_tx, clipboard_hash_for_listener)
    {
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
                                let encrypted = match net::secure_input::seal_payload(
                                    &packet.token,
                                    b"clipboard",
                                    &bytes,
                                    64 * 1024 * 1024 + 1024,
                                ) {
                                    Ok(encrypted) => encrypted,
                                    Err(_) => continue,
                                };
                                let len = (encrypted.len() as u32).to_be_bytes();
                                let target_addr = format!("{}:48102", ip);
                                if let Ok(mut stream) = std::net::TcpStream::connect_timeout(
                                    &target_addr.parse().unwrap(),
                                    Duration::from_millis(800),
                                ) {
                                    use std::io::Write;
                                    let _ = stream.write_all(&len);
                                    let _ = stream.write_all(&encrypted);
                                    let _ = stream.flush();
                                }
                            }
                        }
                        clipboard::ClipboardPayload::Image { format, bytes } => {
                            if bytes.len() > 64 * 1024 * 1024 || token.len() > 256 {
                                continue;
                            }
                            let mut packet = Vec::with_capacity(10 + token.len() + bytes.len());
                            packet.extend_from_slice(b"NCBI");
                            packet.extend_from_slice(&(token.len() as u16).to_be_bytes());
                            packet.extend_from_slice(token.as_bytes());
                            packet.extend_from_slice(&format.to_be_bytes());
                            packet.extend_from_slice(&bytes);
                            let encrypted = match net::secure_input::seal_payload(
                                &token,
                                b"clipboard",
                                &packet,
                                64 * 1024 * 1024 + 1024,
                            ) {
                                Ok(encrypted) => encrypted,
                                Err(_) => continue,
                            };
                            let target_addr = format!("{}:48102", ip);
                            if let Ok(mut stream) = std::net::TcpStream::connect_timeout(
                                &target_addr.parse().unwrap(),
                                Duration::from_millis(800),
                            ) {
                                use std::io::Write;
                                let _ = stream.write_all(&(encrypted.len() as u32).to_be_bytes());
                                let _ = stream.write_all(&encrypted);
                                let _ = stream.flush();
                            }
                        }
                        _ => {}
                    }
                }
            }
        })
        .expect("Failed to spawn clipboard sender thread");
}

fn spawn_video_receiver(state: &AppState) {
    let live_fps_for_vid_rx = state.received_video_fps_x10.clone();
    let viewer_for_vid_rx = state.projection_viewer.clone();
    let paired_peer_ip = state.paired_peer_ip.clone();
    let paired_key = state.paired_security_key.clone();
    engine_async_spawn(async move {
        let bind_addr: SocketAddr = "0.0.0.0:48101".parse().unwrap();
        let mut video_receiver =
            match VideoReceiver::bind(bind_addr, paired_peer_ip, paired_key).await {
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
                            viewer_for_vid_rx.present_frame(
                                decoded.width,
                                decoded.height,
                                0,
                                0,
                                decoded.bgra,
                            );
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
}

fn spawn_mirror_receiver(state: &AppState) {
    let mirror_session = Arc::new(tokio::sync::Mutex::new(()));
    for codec in [MirrorCodec::Av1, MirrorCodec::H264, MirrorCodec::Jpeg] {
        let paired_ip = state.paired_peer_ip.clone();
        let paired_key = state.paired_security_key.clone();
        let viewer_for_mirror = state.projection_viewer.clone();
        let fps_for_mirror = state.received_mirror_fps_x10.clone();
        let mirror_session = mirror_session.clone();
        engine_async_spawn(async move {
            let receiver = match MirrorReceiver::bind(codec).await {
                Ok(receiver) => receiver,
                Err(e) => {
                    error!("Mirror receiver cannot listen on TCP {}: {e}", codec.port());
                    return;
                }
            };
            loop {
                let (stream, peer_addr) = match receiver.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        warn!("Mirror accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let allowed = paired_ip.lock().ok().and_then(|ip| ip.clone());
                if allowed.as_deref() != Some(peer_addr.ip().to_string().as_str()) {
                    continue;
                }
                let key = paired_key.lock().ok().and_then(|key| key.clone());
                let Some(key) = key else { continue };
                let _session_guard = mirror_session.lock().await;

                let (mut read_half, mut write_half) = stream.into_split();
                let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

                let decode_worker = if codec == MirrorCodec::Av1 {
                    let (frame_tx, mut frame_rx) =
                        tokio::sync::mpsc::channel::<(u32, u32, i16, i16, Vec<u8>)>(2);
                    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
                    let viewer = viewer_for_mirror.clone();
                    let worker = tokio::task::spawn_blocking(move || {
                        let apartment = match codec::av1::Apartment::new() {
                            Ok(apartment) => apartment,
                            Err(e) => {
                                let _ = ready_tx.send(Err(e));
                                return;
                            }
                        };
                        let _apartment = apartment;
                        if let Err(e) = codec::av1::Decoder::new(64, 64) {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                        let _ = ready_tx.send(Ok(()));
                        let mut decoder = None;
                        let mut size = (0, 0);
                        while let Some((width, height, ox, oy, bytes)) = frame_rx.blocking_recv() {
                            if size != (width, height) {
                                match codec::av1::Decoder::new(width, height) {
                                    Ok(new_decoder) => {
                                        decoder = Some(new_decoder);
                                        size = (width, height);
                                    }
                                    Err(e) => {
                                        warn!("AV1 decoder initialization failed: {e}");
                                        break;
                                    }
                                }
                            }
                            match decoder.as_mut().unwrap().decode(&bytes) {
                                Ok(Some(bgra)) => viewer.present_frame(width, height, ox, oy, bgra),
                                Ok(None) => {}
                                Err(e) => {
                                    warn!("AV1 decode failed: {e}");
                                    break;
                                }
                            }
                        }
                    });
                    if !matches!(ready_rx.await, Ok(Ok(()))) {
                        continue;
                    }
                    Some((frame_tx, worker))
                } else if codec == MirrorCodec::H264 {
                    let (frame_tx, mut frame_rx) =
                        tokio::sync::mpsc::channel::<(u32, u32, i16, i16, Vec<u8>)>(2);
                    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
                    let viewer = viewer_for_mirror.clone();
                    let worker = tokio::task::spawn_blocking(move || {
                        let apartment = match codec::h264::Apartment::new() {
                            Ok(apartment) => apartment,
                            Err(e) => {
                                let _ = ready_tx.send(Err(e));
                                return;
                            }
                        };
                        let _apartment = apartment;
                        if let Err(e) = codec::h264::Decoder::new(64, 64) {
                            let _ = ready_tx.send(Err(e));
                            return;
                        }
                        let _ = ready_tx.send(Ok(()));
                        let mut decoder = None;
                        let mut size = (0, 0);
                        while let Some((width, height, ox, oy, bytes)) = frame_rx.blocking_recv() {
                            if size != (width, height) {
                                match codec::h264::Decoder::new(width, height) {
                                    Ok(new_decoder) => {
                                        decoder = Some(new_decoder);
                                        size = (width, height);
                                    }
                                    Err(e) => {
                                        warn!("H.264 decoder initialization failed: {e}");
                                        break;
                                    }
                                }
                            }
                            match decoder.as_mut().unwrap().decode(&bytes) {
                                Ok(Some(bgra)) => viewer.present_frame(width, height, ox, oy, bgra),
                                Ok(None) => {}
                                Err(e) => {
                                    warn!("H.264 decode failed: {e}");
                                    break;
                                }
                            }
                        }
                    });
                    if !matches!(ready_rx.await, Ok(Ok(()))) {
                        continue;
                    }
                    Some((frame_tx, worker))
                } else {
                    None
                };

                if codec == MirrorCodec::Av1 {
                    use tokio::io::AsyncWriteExt;
                    if write_half.write_all(&[0x41]).await.is_err() {
                        continue;
                    }
                } else if codec == MirrorCodec::H264 {
                    use tokio::io::AsyncWriteExt;
                    if write_half.write_all(&[0x48]).await.is_err() {
                        continue;
                    }
                }

                let feedback_key = key.clone();
                let write_task = tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    while let Some(msg) = input_rx.recv().await {
                        let wire = if msg == [0x06] {
                            msg
                        } else {
                            let encrypted = match net::secure_input::seal_payload(
                                &feedback_key,
                                b"mirror-feedback",
                                &msg,
                                2048,
                            ) {
                                Ok(encrypted) => encrypted,
                                Err(_) => continue,
                            };
                            let mut wire = Vec::with_capacity(encrypted.len() + 3);
                            wire.push(0xA1);
                            wire.extend_from_slice(&(encrypted.len() as u16).to_le_bytes());
                            wire.extend_from_slice(&encrypted);
                            wire
                        };
                        if write_half.write_all(&wire).await.is_err() {
                            break;
                        }
                    }
                });

                viewer_for_mirror.set_input_channel(Some(input_tx.clone()));
                input::disable_edge_detection();
                let mut frames = 0u64;
                let mut replay = net::secure_input::ReplayGuard::new();
                let mut last_id = 0u32;
                let mut report = Instant::now();
                loop {
                    let (width, height, offset_x, offset_y, payload) = match tokio::time::timeout(
                        Duration::from_millis(1000),
                        MirrorReceiver::read_frame(
                            &mut read_half,
                            codec,
                            &key,
                            &mut replay,
                            &mut last_id,
                        ),
                    )
                    .await
                    {
                        Ok(Ok(frame)) => frame,
                        _ => break,
                    };

                    if let Some((ref frame_tx, _)) = decode_worker {
                        if frame_tx
                            .send((width, height, offset_x, offset_y, payload))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    } else {
                        let viewer = viewer_for_mirror.clone();
                        tokio::task::spawn_blocking(move || {
                            match decode_mirror_bgra(width, height, &payload) {
                                Ok(bgra) => {
                                    viewer.present_frame(width, height, offset_x, offset_y, bgra)
                                }
                                Err(e) => warn!("Mirror JPEG decode failed: {e}"),
                            }
                        });
                    }

                    let _ = input_tx.send(vec![0x06]);
                    frames += 1;

                    if report.elapsed() >= Duration::from_secs(1) {
                        fps_for_mirror.store(
                            (frames as f64 / report.elapsed().as_secs_f64() * 10.0) as u64,
                            Ordering::Relaxed,
                        );
                        frames = 0;
                        report = Instant::now();
                    }
                }
                if let Some((frame_tx, worker)) = decode_worker {
                    drop(frame_tx);
                    let _ = worker.await;
                }
                viewer_for_mirror.set_input_channel(None);
                viewer_for_mirror.stop();
                write_task.abort();
                fps_for_mirror.store(0, Ordering::Relaxed);
                input::restore_edge_detection();
            }
        });
    }
}

static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

pub fn engine_async_spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(fut)
    } else {
        let rt = RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("Failed to initialize Tokio runtime for NerveEngine")
        });
        rt.spawn(fut)
    }
}
