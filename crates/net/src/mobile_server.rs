use common::{ButtonState, MouseButton};
use input::InputEmulator;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{error, info};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use windows::Win32::Security::Cryptography::{BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

pub struct MobileServer {
    is_running: Arc<AtomicBool>,
    port: u16,
    pin: Arc<Mutex<String>>,
    tokens: Arc<Mutex<HashMap<String, Instant>>>,
    pin_attempts: Arc<Mutex<HashMap<std::net::IpAddr, (u8, Instant)>>>,
}

#[derive(Debug, Deserialize)]
pub struct MobileInputPayload {
    pub token: Option<String>,
    pub action: String,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub dx: Option<i32>,
    pub dy: Option<i32>,
    pub button: Option<String>,
    pub delta: Option<i32>,
    pub text: Option<String>,
    pub key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VerifyPinPayload {
    pub pin: String,
}

fn generate_random_pin() -> String {
    let mut bytes = [0u8; 4];
    unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
        .ok()
        .expect("Windows random generator failed");
    let val = u32::from_le_bytes(bytes) % 900000 + 100000;
    format!("{val:06}")
}

fn generate_session_token() -> String {
    let mut bytes = [0u8; 32];
    unsafe { BCryptGenRandom(None, &mut bytes, BCRYPT_USE_SYSTEM_PREFERRED_RNG) }
        .ok()
        .expect("Windows random generator failed");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn get_local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

pub fn get_screen_resolution() -> (i32, i32) {
    unsafe {
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        (w, h)
    }
}

pub fn capture_screen_jpeg(quality: u8) -> Option<Vec<u8>> {
    unsafe {
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);
        if screen_w <= 0 || screen_h <= 0 {
            return None;
        }

        let scale = if screen_w >= 1920 { 2 } else { 1 };
        let target_w = (screen_w / scale) as u32;
        let target_h = (screen_h / scale) as u32;

        let hdc_screen = GetDC(None);
        let hdc_mem = CreateCompatibleDC(hdc_screen);
        let hbm = CreateCompatibleBitmap(hdc_screen, screen_w, screen_h);
        let old_bm = SelectObject(hdc_mem, hbm);

        let _ = BitBlt(hdc_mem, 0, 0, screen_w, screen_h, hdc_screen, 0, 0, SRCCOPY);

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: screen_w,
                biHeight: -screen_h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut raw_buffer = vec![0u8; (screen_w * screen_h * 4) as usize];
        let lines = GetDIBits(
            hdc_mem,
            hbm,
            0,
            screen_h as u32,
            Some(raw_buffer.as_mut_ptr() as _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        SelectObject(hdc_mem, old_bm);
        let _ = DeleteObject(hbm);
        let _ = DeleteDC(hdc_mem);
        let _ = ReleaseDC(None, hdc_screen);

        if lines == 0 {
            return None;
        }

        let mut rgb = Vec::with_capacity((target_w * target_h * 3) as usize);
        let stride = (screen_w * 4) as usize;

        for y in 0..target_h {
            let src_y = (y * scale as u32) as usize;
            let row_offset = src_y * stride;
            for x in 0..target_w {
                let src_x = (x * scale as u32) as usize;
                let pixel_offset = row_offset + src_x * 4;
                if pixel_offset + 3 <= raw_buffer.len() {
                    let b = raw_buffer[pixel_offset];
                    let g = raw_buffer[pixel_offset + 1];
                    let r = raw_buffer[pixel_offset + 2];
                    rgb.push(r);
                    rgb.push(g);
                    rgb.push(b);
                } else {
                    rgb.push(0);
                    rgb.push(0);
                    rgb.push(0);
                }
            }
        }

        let mut jpeg_bytes = Vec::new();
        let encoder = jpeg_encoder::Encoder::new(&mut jpeg_bytes, quality);
        if encoder
            .encode(
                &rgb,
                target_w as u16,
                target_h as u16,
                jpeg_encoder::ColorType::Rgb,
            )
            .is_ok()
        {
            Some(jpeg_bytes)
        } else {
            None
        }
    }
}

impl MobileServer {
    pub fn new(port: u16) -> Self {
        Self {
            is_running: Arc::new(AtomicBool::new(false)),
            port,
            pin: Arc::new(Mutex::new(generate_random_pin())),
            tokens: Arc::new(Mutex::new(HashMap::new())),
            pin_attempts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn get_pin(&self) -> String {
        self.pin.lock().unwrap().clone()
    }

    pub fn regenerate_pin(&self) -> String {
        let new_p = generate_random_pin();
        *self.pin.lock().unwrap() = new_p.clone();
        self.tokens.lock().unwrap().clear();
        self.pin_attempts.lock().unwrap().clear();
        new_p
    }

    pub fn start(&self) {
        if self.is_running.swap(true, Ordering::SeqCst) {
            return;
        }

        let is_running = self.is_running.clone();
        let port = self.port;
        let pin = self.pin.clone();
        let tokens = self.tokens.clone();
        let pin_attempts = self.pin_attempts.clone();

        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("Failed to create Tokio runtime for Mobile Remote: {}", e);
                    is_running.store(false, Ordering::SeqCst);
                    return;
                }
            };

            rt.block_on(async move {
                let local_ip = match get_local_ip().parse() {
                    Ok(ip) => ip,
                    Err(_) => {
                        error!("Mobile Remote could not determine its local IP");
                        is_running.store(false, Ordering::SeqCst);
                        return;
                    }
                };
                let tls = match crate::mobile_tls::server_config(local_ip) {
                    Ok(config) => TlsAcceptor::from(config),
                    Err(e) => {
                        error!("Mobile Remote TLS setup failed: {e}");
                        is_running.store(false, Ordering::SeqCst);
                        return;
                    }
                };
                let addr = SocketAddr::from(([0, 0, 0, 0], port));
                let listener = match TcpListener::bind(addr).await {
                    Ok(l) => {
                        info!("Mobile Remote HTTPS server listening on https://{}", addr);
                        l
                    }
                    Err(e) => {
                        error!("Failed to bind Mobile Remote server: {}", e);
                        is_running.store(false, Ordering::SeqCst);
                        return;
                    }
                };

                let emulator = Arc::new(InputEmulator::new());

                while is_running.load(Ordering::SeqCst) {
                    match tokio::time::timeout(Duration::from_millis(250), listener.accept()).await
                    {
                        Ok(Ok((socket, peer))) => {
                            let emulator = emulator.clone();
                            let running_flag = is_running.clone();
                            let pin_ref = pin.clone();
                            let tokens_ref = tokens.clone();
                            let attempts_ref = pin_attempts.clone();
                            let tls = tls.clone();
                            tokio::spawn(async move {
                                let Ok(Ok(tls_stream)) = tokio::time::timeout(
                                    Duration::from_secs(5),
                                    tls.accept(socket),
                                )
                                .await
                                else {
                                    return;
                                };
                                let _ = handle_http_client(
                                    tls_stream,
                                    peer.ip(),
                                    emulator,
                                    running_flag,
                                    pin_ref,
                                    tokens_ref,
                                    attempts_ref,
                                )
                                .await;
                            });
                        }
                        Ok(Err(_)) => {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        Err(_) => {}
                    }
                }
            });
        });
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
        self.tokens.lock().unwrap().clear();
    }

    pub fn is_active(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn certificate_fingerprint(&self) -> Result<String, String> {
        let ip = get_local_ip()
            .parse()
            .map_err(|e| format!("Local IP: {e}"))?;
        crate::mobile_tls::fingerprint(ip)
    }
}

fn extract_query_param(path: &str, param: &str) -> Option<String> {
    if let Some(pos) = path.find('?') {
        let query = &path[pos + 1..];
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                if k == param {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

async fn read_http_request<S: AsyncRead + Unpin>(
    stream: &mut S,
    pending: &mut Vec<u8>,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    loop {
        if let Some(header_end) = pending.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let header_end = header_end + 4;
            let headers = String::from_utf8_lossy(&pending[..header_end]);
            let content_length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>())
                .transpose()?
                .unwrap_or(0);
            let total = header_end + content_length;
            if total > 8192 {
                return Err("Mobile request is too large".into());
            }
            if pending.len() >= total {
                let request = pending.drain(..total).collect::<Vec<_>>();
                return Ok(Some(String::from_utf8_lossy(&request).into_owned()));
            }
        }
        if pending.len() >= 8192 {
            return Err("Mobile request is too large".into());
        }
        let mut chunk = [0u8; 2048];
        let count =
            tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk)).await??;
        if count == 0 {
            return Ok(None);
        }
        pending.extend_from_slice(&chunk[..count]);
    }
}

async fn handle_http_client<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    peer_ip: std::net::IpAddr,
    emulator: Arc<InputEmulator>,
    running: Arc<AtomicBool>,
    pin: Arc<Mutex<String>>,
    tokens: Arc<Mutex<HashMap<String, Instant>>>,
    pin_attempts: Arc<Mutex<HashMap<std::net::IpAddr, (u8, Instant)>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut pending = Vec::with_capacity(2048);
    loop {
        if !running.load(Ordering::SeqCst) {
            return Ok(());
        }
        let Some(req_str) = read_http_request(&mut stream, &mut pending).await? else {
            return Ok(());
        };
        let mut lines = req_str.lines();
        let first_line = lines.next().unwrap_or("");
        let parts: Vec<&str> = first_line.split_whitespace().collect();

        if parts.len() < 2 {
            return Ok(());
        }

        let method = parts[0];
        let full_path = parts[1];
        let path = full_path.split('?').next().unwrap_or(full_path);

        let token_query = extract_query_param(full_path, "token");
        let mut header_token = None;
        for l in req_str.lines() {
            if l.to_lowercase().starts_with("x-auth-token:") {
                header_token = l.splitn(2, ':').nth(1).map(|s| s.trim().to_string());
                break;
            }
        }

        let request_token = token_query.or(header_token);

        let is_authorized = |tok: &Option<String>| -> bool {
            if let Some(t) = tok {
                let guard = tokens.lock().unwrap();
                guard
                    .get(t)
                    .is_some_and(|created| created.elapsed() < Duration::from_secs(3600))
            } else {
                false
            }
        };

        if method == "GET" && (path == "/" || path == "/index.html") {
            let html = include_str!("mobile.html");
            let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            html.as_bytes().len(),
            html
        );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;
            return Ok(());
        }

        if method == "GET" && path == "/api/status" {
            let (w, h) = get_screen_resolution();
            let auth = is_authorized(&request_token);

            let json = if auth {
                let ip = get_local_ip();
                format!(
                    r#"{{"width":{},"height":{},"ip":"{}","authorized":true}}"#,
                    w, h, ip
                )
            } else {
                format!(r#"{{"width":{},"height":{},"authorized":false}}"#, w, h)
            };
            let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
            json.as_bytes().len(),
            json
        );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;
            return Ok(());
        }

        if method == "POST" && path == "/api/verify" {
            let mut ok = false;
            let mut token = String::new();
            let blocked =
                pin_attempts
                    .lock()
                    .unwrap()
                    .get(&peer_ip)
                    .is_some_and(|(count, since)| {
                        *count >= 5 && since.elapsed() < Duration::from_secs(300)
                    });
            if let Some(body_start) = req_str.find("\r\n\r\n") {
                let body = &req_str[body_start + 4..];
                if let Ok(payload) = serde_json::from_str::<VerifyPinPayload>(body) {
                    let current_pin = pin.lock().unwrap().clone();
                    if !blocked && payload.pin.trim() == current_pin {
                        ok = true;
                        token = generate_session_token();
                        tokens.lock().unwrap().insert(token.clone(), Instant::now());
                        pin_attempts.lock().unwrap().remove(&peer_ip);
                    }
                }
            }
            if !ok && !blocked {
                let mut attempts = pin_attempts.lock().unwrap();
                let entry = attempts.entry(peer_ip).or_insert((0, Instant::now()));
                if entry.1.elapsed() >= Duration::from_secs(300) {
                    *entry = (0, Instant::now());
                }
                entry.0 = entry.0.saturating_add(1);
            }

            let resp_body = if ok {
                format!(r#"{{"ok":true,"token":"{}"}}"#, token)
            } else {
                r#"{"ok":false,"error":"Invalid PIN"}"#.to_string()
            };

            let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
            resp_body.as_bytes().len(),
            resp_body
        );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;
            return Ok(());
        }

        if method == "GET" && path == "/screen.jpg" {
            if !is_authorized(&request_token) {
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\nUnauthorized").await;
                let _ = stream.flush().await;
                let _ = stream.shutdown().await;
                return Ok(());
            }

            if let Ok(Some(jpeg)) = tokio::task::spawn_blocking(|| capture_screen_jpeg(65)).await {
                let response_header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nCache-Control: no-cache, no-store\r\nConnection: close\r\n\r\n",
                jpeg.len()
            );
                let _ = stream.write_all(response_header.as_bytes()).await;
                let _ = stream.write_all(&jpeg).await;
                let _ = stream.flush().await;
                let _ = stream.shutdown().await;
            } else {
                let _ = stream
                    .write_all(b"HTTP/1.1 500 Internal Error\r\nConnection: close\r\n\r\n")
                    .await;
                let _ = stream.flush().await;
                let _ = stream.shutdown().await;
            }
            return Ok(());
        }

        if method == "GET" && path == "/stream" {
            if !is_authorized(&request_token) {
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\nUnauthorized").await;
                let _ = stream.flush().await;
                let _ = stream.shutdown().await;
                return Ok(());
            }

            let mjpeg_header = "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\nCache-Control: no-cache, no-store, must-revalidate\r\nPragma: no-cache\r\nConnection: close\r\n\r\n";
            if stream.write_all(mjpeg_header.as_bytes()).await.is_err() {
                return Ok(());
            }

            while running.load(Ordering::SeqCst) {
                if let Ok(Some(jpeg)) =
                    tokio::task::spawn_blocking(|| capture_screen_jpeg(55)).await
                {
                    let part_header = format!(
                        "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                        jpeg.len()
                    );
                    if stream.write_all(part_header.as_bytes()).await.is_err() {
                        break;
                    }
                    if stream.write_all(&jpeg).await.is_err() {
                        break;
                    }
                    if stream.write_all(b"\r\n").await.is_err() {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(45)).await;
            }
            return Ok(());
        }

        if method == "POST" && path == "/api/input" {
            let mut allowed = is_authorized(&request_token);
            if let Some(body_start) = req_str.find("\r\n\r\n") {
                let body = &req_str[body_start + 4..];
                if let Ok(payload) = serde_json::from_str::<MobileInputPayload>(body) {
                    if !allowed {
                        allowed = is_authorized(&payload.token);
                    }
                    if allowed {
                        process_mobile_input(&payload, &emulator);
                    }
                }
            }

            if !allowed {
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\nUnauthorized").await;
                let _ = stream.flush().await;
                let _ = stream.shutdown().await;
                return Ok(());
            }

            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nAccess-Control-Allow-Origin: *\r\nConnection: keep-alive\r\n\r\nok").await;
            let _ = stream.flush().await;
            continue;
        }

        let _ = stream
            .write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n")
            .await;
        let _ = stream.flush().await;
        let _ = stream.shutdown().await;
        return Ok(());
    }
}

fn process_mobile_input(payload: &MobileInputPayload, emulator: &InputEmulator) {
    match payload.action.as_str() {
        "cursor_pos" => {
            if let (Some(x), Some(y)) = (payload.x, payload.y) {
                let _ = emulator.inject(&common::InputEvent::CursorPosition { x, y });
            }
        }
        "move_by" => {
            if let (Some(dx), Some(dy)) = (payload.dx, payload.dy) {
                let _ = emulator.inject(&common::InputEvent::MouseMove { dx, dy });
            }
        }
        "mouse_down" => {
            let btn = match payload.button.as_deref() {
                Some("right") => MouseButton::Right,
                _ => MouseButton::Left,
            };
            let _ = emulator.inject(&common::InputEvent::MouseButton {
                button: btn,
                state: ButtonState::Pressed,
            });
        }
        "mouse_up" => {
            let btn = match payload.button.as_deref() {
                Some("right") => MouseButton::Right,
                _ => MouseButton::Left,
            };
            let _ = emulator.inject(&common::InputEvent::MouseButton {
                button: btn,
                state: ButtonState::Released,
            });
        }
        "click" => {
            let btn = match payload.button.as_deref() {
                Some("right") => MouseButton::Right,
                _ => MouseButton::Left,
            };
            let _ = emulator.inject(&common::InputEvent::MouseButton {
                button: btn,
                state: ButtonState::Pressed,
            });
            std::thread::sleep(Duration::from_millis(15));
            let _ = emulator.inject(&common::InputEvent::MouseButton {
                button: btn,
                state: ButtonState::Released,
            });
        }
        "scroll" => {
            if let Some(delta) = payload.delta {
                let _ = emulator.inject(&common::InputEvent::MouseWheel {
                    delta: delta as i16,
                    horizontal: false,
                });
            }
        }
        "special" => {
            if let Some(key) = &payload.key {
                match key.as_str() {
                    "double_click" => {
                        let _ = emulator.inject(&common::InputEvent::MouseButton {
                            button: MouseButton::Left,
                            state: ButtonState::Pressed,
                        });
                        std::thread::sleep(Duration::from_millis(15));
                        let _ = emulator.inject(&common::InputEvent::MouseButton {
                            button: MouseButton::Left,
                            state: ButtonState::Released,
                        });
                        std::thread::sleep(Duration::from_millis(50));
                        let _ = emulator.inject(&common::InputEvent::MouseButton {
                            button: MouseButton::Left,
                            state: ButtonState::Pressed,
                        });
                        std::thread::sleep(Duration::from_millis(15));
                        let _ = emulator.inject(&common::InputEvent::MouseButton {
                            button: MouseButton::Left,
                            state: ButtonState::Released,
                        });
                    }
                    "esc" => {
                        inject_key_code(0x1B, emulator);
                    }
                    "enter" => {
                        inject_key_code(0x0D, emulator);
                    }
                    "backspace" => {
                        inject_key_code(0x08, emulator);
                    }
                    "space" => {
                        inject_key_code(0x20, emulator);
                    }
                    "tab" => {
                        inject_key_code(0x09, emulator);
                    }
                    _ => {}
                }
            }
        }
        "type_text" => {
            if let Some(text) = &payload.text {
                for ch in text.chars() {
                    inject_unicode_char(ch);
                }
            }
        }
        _ => {}
    }
}

fn inject_key_code(vk: u16, emulator: &InputEmulator) {
    let _ = emulator.inject(&common::InputEvent::Keyboard {
        vk_code: vk,
        scan_code: 0,
        state: common::KeyState::KeyDown,
        is_extended: false,
    });
    std::thread::sleep(Duration::from_millis(10));
    let _ = emulator.inject(&common::InputEvent::Keyboard {
        vk_code: vk,
        scan_code: 0,
        state: common::KeyState::KeyUp,
        is_extended: false,
    });
}

fn inject_unicode_char(ch: char) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    };

    let mut utf16_buf = [0u16; 2];
    let encoded = ch.encode_utf16(&mut utf16_buf);

    for code_unit in encoded.iter().copied() {
        let input_down = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0),
                    wScan: code_unit,
                    dwFlags: KEYEVENTF_UNICODE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        let input_up = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0),
                    wScan: code_unit,
                    dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            let _ = SendInput(&[input_down, input_up], std::mem::size_of::<INPUT>() as i32);
        }
    }
}

pub fn generate_qr_svg(url: &str) -> Result<String, String> {
    use qrcodegen::{QrCode, QrCodeEcc};
    let qr = QrCode::encode_text(url, QrCodeEcc::Medium)
        .map_err(|e| format!("QR encoding error: {e:?}"))?;

    let border = 2;
    let size = qr.size();
    let total_size = size + border * 2;

    let mut svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {0} {0}" width="180" height="180" shape-rendering="crispEdges">"##,
        total_size
    );
    svg.push_str(r##"<rect width="100%" height="100%" fill="#ffffff" rx="10"/>"##);

    for y in 0..size {
        for x in 0..size {
            if qr.get_module(x, y) {
                let px = x + border;
                let py = y + border;
                svg.push_str(&format!(
                    r##"<rect x="{}" y="{}" width="1" height="1" fill="#0f1115"/>"##,
                    px, py
                ));
            }
        }
    }

    svg.push_str("</svg>");
    Ok(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_multiple_http_requests_without_losing_post_body() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        writer
            .write_all(b"POST /api/input HTTP/1.1\r\nContent-Length: 4\r\n\r\ntestGET /api/status HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut pending = Vec::new();
        let first = read_http_request(&mut reader, &mut pending)
            .await
            .unwrap()
            .unwrap();
        let second = read_http_request(&mut reader, &mut pending)
            .await
            .unwrap()
            .unwrap();
        assert!(first.ends_with("\r\n\r\ntest"));
        assert!(second.starts_with("GET /api/status"));
    }
}
