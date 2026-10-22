use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tracing::{error, warn};

use codec::{HardwareVideoDecoder, VideoDecoderConfig};
use common::InputEvent;
use input::InputEmulator;
use net::{
    decode_mirror_bgra, DeviceRole, DiscoveryBeacon, DiscoveryBeaconEmitter, DiscoveryListener,
    MirrorCodec, MirrorReceiver, UdpInputReceiver, VideoReceiver,
};

use crate::state::{get_screen_dimensions, AppState, ClipboardPacket, DiscoveredDevice};

pub type FileReceivedCallback = Arc<dyn Fn(Vec<String>) + Send + Sync>;

pub fn start_background_services(state: &AppState, on_file_received: Option<FileReceivedCallback>) {
    spawn_discovery_emitter(state);
    spawn_discovery_listener(state);
    spawn_input_receiver(state, on_file_received);
    spawn_clipboard_receiver(state);
    spawn_clipboard_sender(state);
    spawn_video_receiver(state);
    spawn_mirror_receiver(state);
}

fn spawn_discovery_emitter(state: &AppState) {
    let paired_ip_for_disc = state.paired_peer_ip.clone();
    engine_async_spawn(async move {
        let my_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Desktop-PC".into());
        let (sw, sh) = get_screen_dimensions();
        let beacon = DiscoveryBeacon::new(my_name, DeviceRole::Host, 48100, 48101, sw, sh);
        if let Ok(emitter) = DiscoveryBeaconEmitter::new(beacon).await {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            loop {
                ticker.tick().await;
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

    engine_async_spawn(async move {
        if let Ok(listener) = DiscoveryListener::bind().await {
            loop {
                if let Ok((beacon, peer_addr)) = listener.recv_beacon().await {
                    let my_name = std::env::var("COMPUTERNAME").unwrap_or_default();
                    if !my_name.is_empty() && beacon.device_name.eq_ignore_ascii_case(&my_name) {
                        continue;
                    }
                    if peer_addr.port() != 48103 {
                        let (sw, sh) = get_screen_dimensions();
                        let reply =
                            DiscoveryBeacon::new(my_name, DeviceRole::Host, 48100, 48101, sw, sh);
                        let address = SocketAddr::new(peer_addr.ip(), 48103);
                        let _ = listener.reply_to(&reply, address).await;
                    }
                    let ip = peer_addr.ip().to_string();
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
}

fn spawn_input_receiver(state: &AppState, on_file_received: Option<FileReceivedCallback>) {
    let discovered_for_rx = state.discovered_devices.clone();
    let seen_for_rx = state.device_last_seen.clone();
    let peer_device_for_rx = state.peer_device.clone();
    let paired_ip_for_rx = state.paired_peer_ip.clone();
    let keyboard_sharing_for_rx = state.keyboard_sharing.clone();
    let drag_badge_for_rx = state.drag_badge.clone();

    engine_async_spawn(async move {
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
                Ok((_header, event, peer)) => {
                    let peer_ip = peer.ip().to_string();
                    if let Ok(mut seen) = seen_for_rx.lock() {
                        seen.insert(peer_ip.clone(), Instant::now());
                    }

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

                    if let InputEvent::FileDrag {
                        active,
                        ref file_name,
                    } = event
                    {
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
                            let moved = net::commit_file_drop(target_dir);
                            if !moved.is_empty() {
                                if let Some(ref cb) = on_file_received {
                                    let names: Vec<String> = moved
                                        .iter()
                                        .filter_map(|p| {
                                            p.file_name()
                                                .and_then(|n| n.to_str())
                                                .map(|s| s.to_string())
                                        })
                                        .collect();
                                    cb(names);
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
            if let Ok(mut guard) = paired_ip_for_srv.lock() {
                if guard.is_none() {
                    *guard = Some(incoming_ip);
                }
            }

            let hash_ref = clipboard_hash_for_srv.clone();
            let key_ref = paired_key_for_srv.clone();

            tokio::spawn(async move {
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
                            if !key.is_empty()
                                && !clip_packet.token.is_empty()
                                && clip_packet.token != *key
                            {
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
}

fn spawn_video_receiver(state: &AppState) {
    let live_fps_for_vid_rx = state.received_video_fps_x10.clone();
    let viewer_for_vid_rx = state.projection_viewer.clone();
    engine_async_spawn(async move {
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
    for codec in [MirrorCodec::Jpeg, MirrorCodec::H264] {
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
                let (stream, _peer_addr) = match receiver.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        warn!("Mirror accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let _session_guard = mirror_session.lock().await;

                let (mut read_half, mut write_half) = stream.into_split();
                let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

                let decode_worker = if codec == MirrorCodec::H264 {
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

                if codec == MirrorCodec::H264 {
                    use tokio::io::AsyncWriteExt;
                    if write_half.write_all(&[0x48]).await.is_err() {
                        continue;
                    }
                }

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
                    let (width, height, offset_x, offset_y, payload) =
                        match MirrorReceiver::read_frame(&mut read_half, codec).await {
                            Ok(frame) => frame,
                            Err(_) => break,
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
