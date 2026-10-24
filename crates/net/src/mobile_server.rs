use common::{ButtonState, MouseButton};
use input::InputEmulator;
use serde::Deserialize;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
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
    audio_enabled: Arc<AtomicBool>,
    audio_stream_in_use: Arc<AtomicBool>,
    port: u16,
    pin: Arc<Mutex<String>>,
    tokens: Arc<Mutex<HashSet<String>>>,
    pin_attempts: Arc<Mutex<HashMap<std::net::IpAddr, (u8, Instant)>>>,
    share: Arc<Mutex<ShareState>>,
    receive_dir: Arc<Mutex<PathBuf>>,
}

const MAX_SHARE_FILE_BYTES: usize = 100 * 1024 * 1024;
const MAX_SHARE_TEXT_BYTES: usize = 8 * 1024;

#[derive(Clone)]
struct SharedFile {
    id: String,
    path: PathBuf,
    name: String,
    size: u64,
}

#[derive(Default)]
struct ShareState {
    file: Option<SharedFile>,
    text: Option<String>,
    last_received: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct MobileShareInfo {
    pub file_id: Option<String>,
    pub file_name: Option<String>,
    pub file_size: Option<u64>,
    pub text: Option<String>,
    pub last_received: Option<String>,
}

impl ShareState {
    fn info(&self) -> MobileShareInfo {
        MobileShareInfo {
            file_id: self.file.as_ref().map(|file| file.id.clone()),
            file_name: self.file.as_ref().map(|file| file.name.clone()),
            file_size: self.file.as_ref().map(|file| file.size),
            text: self.text.clone(),
            last_received: self.last_received.clone(),
        }
    }
}

struct AudioStreamLease(Arc<AtomicBool>);

impl Drop for AudioStreamLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
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

#[derive(Debug, Deserialize)]
struct ShareTextPayload {
    text: String,
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
            audio_enabled: Arc::new(AtomicBool::new(false)),
            audio_stream_in_use: Arc::new(AtomicBool::new(false)),
            port,
            pin: Arc::new(Mutex::new(generate_random_pin())),
            tokens: Arc::new(Mutex::new(HashSet::new())),
            pin_attempts: Arc::new(Mutex::new(HashMap::new())),
            share: Arc::new(Mutex::new(ShareState::default())),
            receive_dir: Arc::new(Mutex::new(load_receive_dir())),
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
        *self.share.lock().unwrap() = ShareState::default();
        new_p
    }

    pub fn share_info(&self) -> MobileShareInfo {
        self.share.lock().unwrap().info()
    }

    pub fn receive_dir(&self) -> String {
        self.receive_dir
            .lock()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    pub fn set_receive_dir(&self, path: &Path) -> Result<String, String> {
        let dir = path.canonicalize().map_err(|e| e.to_string())?;
        if !dir.is_dir() {
            return Err("Choose an existing folder".into());
        }
        let config = receive_dir_config_path()?;
        std::fs::create_dir_all(config.parent().ok_or("Invalid settings path")?)
            .map_err(|e| e.to_string())?;
        std::fs::write(&config, dir.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        *self.receive_dir.lock().unwrap() = dir;
        Ok(self.receive_dir())
    }

    pub fn reset_receive_dir(&self) -> Result<String, String> {
        let default = mobile_downloads_dir()?;
        std::fs::create_dir_all(&default).map_err(|e| e.to_string())?;
        self.set_receive_dir(&default)
    }

    pub fn share_file(&self, path: &Path) -> Result<MobileShareInfo, String> {
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        let metadata = path.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_SHARE_FILE_BYTES as u64 {
            return Err("Choose a file smaller than 100 MB".into());
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Invalid file name")?
            .to_string();
        let mut share = self.share.lock().map_err(|e| e.to_string())?;
        share.file = Some(SharedFile {
            id: generate_session_token(),
            path,
            name,
            size: metadata.len(),
        });
        Ok(share.info())
    }

    pub fn share_text(&self, text: String) -> Result<MobileShareInfo, String> {
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_SHARE_TEXT_BYTES {
            return Err("Text must be between 1 and 8192 bytes".into());
        }
        let mut share = self.share.lock().map_err(|e| e.to_string())?;
        share.text = Some(text.to_string());
        Ok(share.info())
    }

    pub fn clear_share(&self) -> MobileShareInfo {
        let mut share = self.share.lock().unwrap();
        share.file = None;
        share.text = None;
        share.info()
    }

    pub fn start(&self) {
        if self.is_running.swap(true, Ordering::SeqCst) {
            return;
        }

        let is_running = self.is_running.clone();
        let audio_enabled = self.audio_enabled.clone();
        let audio_stream_in_use = self.audio_stream_in_use.clone();
        let port = self.port;
        let pin = self.pin.clone();
        let tokens = self.tokens.clone();
        let pin_attempts = self.pin_attempts.clone();
        let share = self.share.clone();
        let receive_dir = self.receive_dir.clone();

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
                            let audio_flag = audio_enabled.clone();
                            let audio_stream_flag = audio_stream_in_use.clone();
                            let pin_ref = pin.clone();
                            let tokens_ref = tokens.clone();
                            let attempts_ref = pin_attempts.clone();
                            let share_ref = share.clone();
                            let receive_dir_ref = receive_dir.clone();
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
                                    audio_flag,
                                    audio_stream_flag,
                                    pin_ref,
                                    tokens_ref,
                                    attempts_ref,
                                    share_ref,
                                    receive_dir_ref,
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
        self.audio_enabled.store(false, Ordering::SeqCst);
        *self.share.lock().unwrap() = ShareState::default();
    }

    pub fn set_audio_enabled(&self, enabled: bool) -> bool {
        let active = enabled && self.is_active();
        self.audio_enabled.store(active, Ordering::SeqCst);
        active
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

fn request_header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.trim()))
}

fn decode_file_name(value: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hi = (bytes.next()? as char).to_digit(16)?;
            let lo = (bytes.next()? as char).to_digit(16)?;
            decoded.push(((hi << 4) | lo) as u8);
        } else {
            decoded.push(byte);
        }
    }
    let name = String::from_utf8(decoded).ok()?;
    let name = name.rsplit(['/', '\\']).next()?;
    let safe: String = name
        .chars()
        .take(120)
        .map(|ch| {
            if ch.is_control() || "<>:\"/\\|?*".contains(ch) {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let safe = safe.trim_matches([' ', '.']);
    (!safe.is_empty()).then(|| safe.to_string())
}

pub fn mobile_downloads_dir() -> Result<PathBuf, String> {
    unsafe {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{
            FOLDERID_Downloads, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG,
        };
        if let Ok(path) =
            SHGetKnownFolderPath(&FOLDERID_Downloads, KNOWN_FOLDER_FLAG(0), HANDLE::default())
        {
            let directory = path.to_string().ok().map(PathBuf::from);
            CoTaskMemFree(Some(path.0.cast()));
            if let Some(directory) = directory {
                return Ok(directory);
            }
        }
    }
    let profile = std::env::var_os("USERPROFILE").ok_or("USERPROFILE is unavailable")?;
    Ok(PathBuf::from(profile).join("Downloads"))
}

fn receive_dir_config_path() -> Result<PathBuf, String> {
    let appdata = std::env::var_os("APPDATA").ok_or("APPDATA is unavailable")?;
    Ok(PathBuf::from(appdata)
        .join("NerveDesk")
        .join("mobile-receive-folder.txt"))
}

fn load_receive_dir() -> PathBuf {
    let default = mobile_downloads_dir().unwrap_or_else(|_| PathBuf::from("Downloads"));
    receive_dir_config_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|path| PathBuf::from(path.trim()))
        .and_then(|path| path.canonicalize().ok())
        .filter(|path| path.is_dir())
        .unwrap_or(default)
}

async fn write_json<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: &str,
    value: &impl Serialize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let body = serde_json::to_vec(value)?;
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_http_request<S: AsyncRead + Unpin>(
    stream: &mut S,
    pending: &mut Vec<u8>,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    loop {
        if let Some(header_end) = pending.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let header_end = header_end + 4;
            if header_end > 8192 {
                return Err("Mobile request headers are too large".into());
            }
            let headers = String::from_utf8_lossy(&pending[..header_end]);
            if headers.starts_with("PUT /api/share/upload?") {
                let request = pending.drain(..header_end).collect::<Vec<_>>();
                return Ok(Some(String::from_utf8_lossy(&request).into_owned()));
            }
            let content_length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>())
                .transpose()?
                .unwrap_or(0);
            let max_request = if headers.starts_with("POST /api/share/text ") {
                16 * 1024
            } else {
                8192
            };
            let total = header_end
                .checked_add(content_length)
                .ok_or("Mobile request is too large")?;
            if total > max_request {
                return Err("Mobile request is too large".into());
            }
            if pending.len() >= total {
                let request = pending.drain(..total).collect::<Vec<_>>();
                return Ok(Some(String::from_utf8_lossy(&request).into_owned()));
            }
        }
        if pending.len() >= 16 * 1024 {
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
    audio_enabled: Arc<AtomicBool>,
    audio_stream_in_use: Arc<AtomicBool>,
    pin: Arc<Mutex<String>>,
    tokens: Arc<Mutex<HashSet<String>>>,
    pin_attempts: Arc<Mutex<HashMap<std::net::IpAddr, (u8, Instant)>>>,
    share: Arc<Mutex<ShareState>>,
    receive_dir: Arc<Mutex<PathBuf>>,
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
                guard.contains(t)
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

        if method == "GET"
            && matches!(
                path,
                "/manifest.webmanifest" | "/mobile-icon-192.png" | "/mobile-icon-512.png"
            )
        {
            let (content_type, body): (&str, &[u8]) = match path {
                "/manifest.webmanifest" => (
                    "application/manifest+json; charset=utf-8",
                    include_bytes!("../assets/manifest.webmanifest"),
                ),
                "/mobile-icon-192.png" => (
                    "image/png",
                    include_bytes!("../assets/mobile-icon-192.png"),
                ),
                _ => ("image/png", include_bytes!("../assets/mobile-icon-512.png")),
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).await?;
            stream.write_all(body).await?;
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
                        tokens.lock().unwrap().insert(token.clone());
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

        if path.starts_with("/api/share/") {
            if !is_authorized(&request_token) {
                write_json(
                    &mut stream,
                    "401 Unauthorized",
                    &serde_json::json!({"error":"Session expired"}),
                )
                .await?;
                return Ok(());
            }

            if method == "GET" && path == "/api/share/outbox" {
                let info = share.lock().unwrap().info();
                write_json(&mut stream, "200 OK", &info).await?;
                return Ok(());
            }

            if method == "POST" && path == "/api/share/text" {
                let body = req_str
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body)
                    .unwrap_or("");
                let payload = serde_json::from_str::<ShareTextPayload>(body);
                let Ok(payload) = payload else {
                    write_json(
                        &mut stream,
                        "400 Bad Request",
                        &serde_json::json!({"error":"Invalid text"}),
                    )
                    .await?;
                    return Ok(());
                };
                let text = payload.text.trim().to_string();
                if text.is_empty() || text.len() > MAX_SHARE_TEXT_BYTES {
                    write_json(
                        &mut stream,
                        "413 Content Too Large",
                        &serde_json::json!({"error":"Text is empty or too long"}),
                    )
                    .await?;
                    return Ok(());
                }
                let result = tokio::task::spawn_blocking(move || {
                    clipboard::ClipboardWriter::write_text(&text)
                })
                .await?;
                if let Err(error) = result {
                    write_json(
                        &mut stream,
                        "500 Internal Server Error",
                        &serde_json::json!({"error":error.to_string()}),
                    )
                    .await?;
                    return Ok(());
                }
                share.lock().unwrap().last_received = Some("text".into());
                write_json(&mut stream, "200 OK", &serde_json::json!({"ok":true})).await?;
                return Ok(());
            }

            if method == "PUT" && path == "/api/share/upload" {
                let size = request_header(&req_str, "content-length")
                    .and_then(|s| s.parse::<usize>().ok());
                let name =
                    extract_query_param(full_path, "name").and_then(|s| decode_file_name(&s));
                let (Some(size), Some(name)) = (size, name) else {
                    write_json(
                        &mut stream,
                        "400 Bad Request",
                        &serde_json::json!({"error":"File name or size is missing"}),
                    )
                    .await?;
                    return Ok(());
                };
                if size == 0
                    || size > MAX_SHARE_FILE_BYTES
                    || request_header(&req_str, "transfer-encoding").is_some()
                {
                    write_json(
                        &mut stream,
                        "413 Content Too Large",
                        &serde_json::json!({"error":"File must be 1 byte to 100 MB"}),
                    )
                    .await?;
                    return Ok(());
                }
                let dir = receive_dir.lock().unwrap().clone();
                tokio::fs::create_dir_all(&dir).await?;
                let id = generate_session_token();
                let saved_name = format!("{}_{}", &id[..12], name);
                let target = dir.join(&saved_name);
                let temporary = dir.join(format!(".{id}.part"));
                let mut file = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)
                    .await?;
                let upload_result = async {
                    let mut remaining = size;
                    let buffered = pending.len().min(remaining);
                    file.write_all(&pending[..buffered]).await?;
                    pending.drain(..buffered);
                    remaining -= buffered;
                    let mut chunk = [0u8; 32 * 1024];
                    while remaining > 0 {
                        if !running.load(Ordering::SeqCst) || !is_authorized(&request_token) {
                            return Err("Session expired during upload".into());
                        }
                        let to_read = remaining.min(chunk.len());
                        let count = tokio::time::timeout(
                            Duration::from_secs(30),
                            stream.read(&mut chunk[..to_read]),
                        )
                        .await??;
                        if count == 0 {
                            return Err("Upload ended early".into());
                        }
                        file.write_all(&chunk[..count]).await?;
                        remaining -= count;
                    }
                    file.flush().await?;
                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                }
                .await;
                drop(file);
                if let Err(error) = upload_result {
                    let _ = tokio::fs::remove_file(&temporary).await;
                    write_json(
                        &mut stream,
                        "400 Bad Request",
                        &serde_json::json!({"error":error.to_string()}),
                    )
                    .await?;
                    return Ok(());
                }
                if let Err(error) = tokio::fs::rename(&temporary, &target).await {
                    let _ = tokio::fs::remove_file(&temporary).await;
                    return Err(error.into());
                }
                share.lock().unwrap().last_received = Some(saved_name.clone());
                write_json(
                    &mut stream,
                    "200 OK",
                    &serde_json::json!({"ok":true,"saved_name":saved_name}),
                )
                .await?;
                return Ok(());
            }

            if method == "GET" && path == "/api/share/download" {
                let id = extract_query_param(full_path, "id");
                let selected = share.lock().unwrap().file.clone();
                let Some(file) = selected.filter(|file| id.as_deref() == Some(file.id.as_str()))
                else {
                    write_json(
                        &mut stream,
                        "404 Not Found",
                        &serde_json::json!({"error":"No shared file"}),
                    )
                    .await?;
                    return Ok(());
                };
                let mut source = tokio::fs::File::open(&file.path).await?;
                if source.metadata().await?.len() != file.size {
                    write_json(
                        &mut stream,
                        "409 Conflict",
                        &serde_json::json!({"error":"Shared file changed"}),
                    )
                    .await?;
                    return Ok(());
                }
                let header = format!("HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", file.size);
                stream.write_all(header.as_bytes()).await?;
                let mut chunk = [0u8; 32 * 1024];
                loop {
                    if !running.load(Ordering::SeqCst) || !is_authorized(&request_token) {
                        break;
                    }
                    let count = source.read(&mut chunk).await?;
                    if count == 0 {
                        break;
                    }
                    tokio::time::timeout(
                        Duration::from_secs(30),
                        stream.write_all(&chunk[..count]),
                    )
                    .await??;
                }
                return Ok(());
            }
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

        if method == "GET" && path == "/audio.pcm" {
            if !is_authorized(&request_token) {
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\nUnauthorized").await;
                return Ok(());
            }
            if !audio_enabled.load(Ordering::SeqCst) {
                stream
                    .write_all(
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await?;
                return Ok(());
            }
            if audio_stream_in_use
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                stream
                    .write_all(
                        b"HTTP/1.1 409 Conflict\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await?;
                return Ok(());
            }
            let _lease = AudioStreamLease(audio_stream_in_use.clone());
            let (sender, mut receiver) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
            let capture_running = running.clone();
            let capture_enabled = audio_enabled.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) =
                    crate::audio::capture_for_mobile(sender, capture_running, capture_enabled)
                {
                    tracing::warn!("Mobile audio capture stopped: {e}");
                }
            });
            let Some(format_header) =
                tokio::time::timeout(Duration::from_secs(10), receiver.recv())
                    .await
                    .ok()
                    .flatten()
            else {
                let _ = stream.write_all(b"HTTP/1.1 500 Internal Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                return Ok(());
            };
            let header = b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nCache-Control: no-store\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            tokio::time::timeout(Duration::from_secs(5), stream.write_all(header)).await??;
            let mut first_packet = Some(format_header);
            while running.load(Ordering::SeqCst)
                && audio_enabled.load(Ordering::SeqCst)
                && is_authorized(&request_token)
            {
                let Some(packet) = (if let Some(header) = first_packet.take() {
                    Some(header)
                } else {
                    let mut closed = [0u8; 1];
                    tokio::select! {
                        packet = receiver.recv() => packet,
                        read = stream.read(&mut closed) => {
                            if read.is_err() || read.ok() == Some(0) { break; }
                            continue;
                        }
                        _ = tokio::time::sleep(Duration::from_secs(1)) => continue,
                    }
                }) else {
                    break;
                };
                let size = (packet.len() as u32).to_le_bytes();
                let chunk = [size.as_slice(), packet.as_slice()].concat();
                let write = async {
                    stream
                        .write_all(format!("{:X}\r\n", chunk.len()).as_bytes())
                        .await?;
                    stream.write_all(&chunk).await?;
                    stream.write_all(b"\r\n").await
                };
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(5), write).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n").await;
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

    #[tokio::test]
    async fn upload_request_leaves_binary_body_for_streaming() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        writer
            .write_all(b"PUT /api/share/upload?name=photo.jpg HTTP/1.1\r\nContent-Length: 4\r\n\r\n\0\xff\x80A")
            .await
            .unwrap();
        let mut pending = Vec::new();
        let headers = read_http_request(&mut reader, &mut pending)
            .await
            .unwrap()
            .unwrap();
        assert!(headers.starts_with("PUT /api/share/upload"));
        assert!(!headers.contains('�'));
        assert_eq!(pending, b"\0\xff\x80A");
    }

    #[tokio::test]
    async fn share_text_request_accepts_the_documented_limit() {
        let (mut writer, mut reader) = tokio::io::duplex(16 * 1024);
        let body = format!("{{\"text\":\"{}\"}}", "a".repeat(MAX_SHARE_TEXT_BYTES));
        let request = format!(
            "POST /api/share/text HTTP/1.1\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        writer.write_all(request.as_bytes()).await.unwrap();
        let parsed = read_http_request(&mut reader, &mut Vec::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(parsed, request);
    }

    #[tokio::test]
    async fn share_outbox_requires_a_valid_session() {
        for authorized in [false, true] {
            let (mut client, server) = tokio::io::duplex(4096);
            let token = generate_session_token();
            let tokens = Arc::new(Mutex::new(HashSet::new()));
            if authorized {
                tokens.lock().unwrap().insert(token.clone());
            }
            let shared = Arc::new(Mutex::new(ShareState {
                text: Some("https://example.com".into()),
                ..ShareState::default()
            }));
            let task = tokio::spawn(handle_http_client(
                server,
                "127.0.0.1".parse().unwrap(),
                Arc::new(InputEmulator::new()),
                Arc::new(AtomicBool::new(true)),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new("123456".into())),
                tokens,
                Arc::new(Mutex::new(HashMap::new())),
                shared,
                Arc::new(Mutex::new(mobile_downloads_dir().unwrap())),
            ));
            client
                .write_all(
                    format!("GET /api/share/outbox HTTP/1.1\r\nX-Auth-Token: {token}\r\n\r\n")
                        .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = Vec::new();
            client.read_to_end(&mut response).await.unwrap();
            task.await.unwrap().unwrap();
            let response = String::from_utf8(response).unwrap();
            if authorized {
                assert!(response.starts_with("HTTP/1.1 200 OK"));
                assert!(response.contains("https://example.com"));
            } else {
                assert!(response.starts_with("HTTP/1.1 401 Unauthorized"));
                assert!(!response.contains("https://example.com"));
            }
        }
    }

    #[test]
    fn upload_name_cannot_escape_downloads_folder() {
        assert_eq!(
            decode_file_name("..%5c..%2fphoto%3a.jpg"),
            Some("photo_.jpg".into())
        );
        assert_eq!(decode_file_name("%2e%2e"), None);
        assert_eq!(decode_file_name("%FF"), None);
    }

    #[test]
    fn remembered_phone_survives_service_toggle_but_not_pin_change_or_restart() {
        let server = MobileServer::new(5858);
        let token = generate_session_token();
        server.tokens.lock().unwrap().insert(token.clone());
        server.stop();
        assert!(server.tokens.lock().unwrap().contains(&token));
        server.regenerate_pin();
        assert!(!server.tokens.lock().unwrap().contains(&token));
        assert!(MobileServer::new(5858).tokens.lock().unwrap().is_empty());
    }
}
