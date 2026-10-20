use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info, warn};
use common::{NerveError, Result};

pub const FILE_TRANSFER_PORT: u16 = 48104;
const FILE_TRANSFER_MAGIC: &[u8; 4] = b"NRVF";
const CHUNK_SIZE: usize = 65536;

pub fn get_default_transfer_dir() -> PathBuf {
    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        let desktop = PathBuf::from(&user_profile).join("Desktop");
        if desktop.exists() {
            return desktop;
        }
        let downloads = PathBuf::from(&user_profile).join("Downloads");
        if downloads.exists() {
            return downloads;
        }
        PathBuf::from(user_profile)
    } else {
        std::env::temp_dir()
    }
}

fn url_to_path(url: &str) -> Option<PathBuf> {
    let clean = url.trim();
    if let Some(stripped) = clean.strip_prefix("file:///") {
        let decoded = url_decode(stripped);
        let path_str = decoded.replace('/', "\\");
        let path = PathBuf::from(path_str);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(c1), Some(c2)) = (h1, h2) {
                let hex_str = format!("{c1}{c2}");
                if let Ok(byte) = u8::from_str_radix(&hex_str, 16) {
                    out.push(byte as char);
                    continue;
                }
            }
        }
        out.push(c);
    }
    out
}

pub fn get_target_dir_at_cursor() -> PathBuf {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{
            GetAncestor, GetClassNameW, GetCursorPos, WindowFromPoint, GA_ROOT,
        };
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Shell::{IShellWindows, IWebBrowser2, ShellWindows};
        use windows::core::{Interface, VARIANT};

        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_ok() {
            let hwnd = WindowFromPoint(pt);
            if !hwnd.is_invalid() {
                let root = GetAncestor(hwnd, GA_ROOT);
                let target_hwnd = if root.is_invalid() { hwnd } else { root };

                let mut class_buf = [0u16; 256];
                let len = GetClassNameW(target_hwnd, &mut class_buf);
                let class_name = String::from_utf16_lossy(&class_buf[..len as usize]);

                if class_name == "Progman" || class_name == "WorkerW" {
                    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
                        let desktop = PathBuf::from(user_profile).join("Desktop");
                        if desktop.exists() {
                            return desktop;
                        }
                    }
                } else if class_name == "CabinetWClass" || class_name == "ExploreWClass" {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                    if let Ok(sw) = CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_ALL) {
                        if let Ok(count) = sw.Count() {
                            for i in 0..count {
                                let v = VARIANT::from(i);
                                if let Ok(item) = sw.Item(&v) {
                                    if let Ok(wb) = item.cast::<IWebBrowser2>() {
                                        if let Ok(whwnd) = wb.HWND() {
                                            if whwnd.0 as isize == target_hwnd.0 as isize {
                                                if let Ok(url) = wb.LocationURL() {
                                                    if let Some(path) = url_to_path(&url.to_string()) {
                                                        return path;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    get_default_transfer_dir()
}

fn get_unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let stem = Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = Path::new(name).extension().and_then(|s| s.to_str());
    for i in 1..10000 {
        let new_name = match ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        };
        let candidate = dir.join(new_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    p
}

pub struct FileTransferServer {
    is_running: Arc<AtomicBool>,
}

impl FileTransferServer {
    pub async fn start<F>(
        port: u16,
        security_key: Arc<std::sync::Mutex<Option<String>>>,
        enabled: Arc<AtomicBool>,
        on_files_received: F,
    ) -> Result<Self>
    where
        F: Fn(Vec<PathBuf>) + Send + Sync + Clone + 'static,
    {
        let listener = TcpListener::bind(format!("0.0.0.0:{port}")).await?;

        let is_running = Arc::new(AtomicBool::new(true));
        let running_clone = is_running.clone();

        tokio::spawn(async move {
            info!("File transfer server listening on port {port}");
            while running_clone.load(Ordering::Relaxed) {
                match listener.accept().await {
                    Ok((socket, addr)) => {
                        if !enabled.load(Ordering::Relaxed) {
                            continue;
                        }
                        info!("Incoming file transfer connection from {addr}");
                        let dir = get_target_dir_at_cursor();
                        let key = security_key.clone();
                        let cb = on_files_received.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_file_connection(socket, dir, key, cb).await {
                                warn!("File transfer failed: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        if !running_clone.load(Ordering::Relaxed) {
                            break;
                        }
                        error!("Accept failed on file transfer socket: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                }
            }
        });

        Ok(Self { is_running })
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}

pub async fn handle_file_connection<F>(
    mut socket: TcpStream,
    target_dir: PathBuf,
    security_key: Arc<std::sync::Mutex<Option<String>>>,
    on_files_received: F,
) -> Result<()>
where
    F: Fn(Vec<PathBuf>) + Send + Sync + 'static,
{
    let mut magic = [0u8; 4];
    socket.read_exact(&mut magic).await?;
    if &magic != FILE_TRANSFER_MAGIC {
        return Err(NerveError::Protocol("Invalid file transfer magic".into()));
    }

    let mut token_len_buf = [0u8; 2];
    socket.read_exact(&mut token_len_buf).await?;
    let token_len = u16::from_be_bytes(token_len_buf) as usize;
    if token_len > 256 {
        return Err(NerveError::Protocol("Token too large".into()));
    }
    let mut token_buf = vec![0u8; token_len];
    socket.read_exact(&mut token_buf).await?;
    let token = String::from_utf8_lossy(&token_buf);

    if let Ok(guard) = security_key.lock() {
        if let Some(ref key) = *guard {
            if !key.is_empty() && !token.is_empty() && token != *key {
                return Err(NerveError::Protocol("Security key mismatch for file transfer".into()));
            }
        }
    }

    let mut count_buf = [0u8; 4];
    socket.read_exact(&mut count_buf).await?;
    let file_count = u32::from_be_bytes(count_buf) as usize;
    if file_count == 0 || file_count > 1000 {
        return Err(NerveError::Protocol("Invalid file count".into()));
    }

    let _ = std::fs::create_dir_all(&target_dir);
    let mut received_files = Vec::new();

    for _ in 0..file_count {
        let mut name_len_buf = [0u8; 2];
        socket.read_exact(&mut name_len_buf).await?;
        let name_len = u16::from_be_bytes(name_len_buf) as usize;
        if name_len == 0 || name_len > 1024 {
            return Err(NerveError::Protocol("Invalid filename length".into()));
        }
        let mut name_buf = vec![0u8; name_len];
        socket.read_exact(&mut name_buf).await?;
        let raw_name = String::from_utf8_lossy(&name_buf);
        let safe_name = Path::new(&*raw_name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("received_file");

        let mut size_buf = [0u8; 8];
        socket.read_exact(&mut size_buf).await?;
        let file_size = u64::from_be_bytes(size_buf);

        let final_path = get_unique_path(&target_dir, safe_name);
        let final_name = final_path.file_name().and_then(|n| n.to_str()).unwrap_or(safe_name);
        let part_path = target_dir.join(format!("{final_name}.part"));

        let mut out_file = tokio::fs::File::create(&part_path).await?;

        let mut remaining = file_size;
        let mut buffer = vec![0u8; CHUNK_SIZE];
        while remaining > 0 {
            let to_read = std::cmp::min(remaining as usize, CHUNK_SIZE);
            socket.read_exact(&mut buffer[..to_read]).await?;
            out_file.write_all(&buffer[..to_read]).await?;
            remaining -= to_read as u64;
        }

        out_file.flush().await?;
        drop(out_file);

        let _ = tokio::fs::remove_file(&final_path).await;
        tokio::fs::rename(&part_path, &final_path).await?;

        #[cfg(target_os = "windows")]
        unsafe {
            use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
            use windows::core::s;
            if let Ok(shell32) = LoadLibraryA(s!("shell32.dll")) {
                if let Some(proc) = GetProcAddress(shell32, s!("SHChangeNotify")) {
                    type SHChangeNotifyFn = unsafe extern "system" fn(i32, u32, *const u16, *const u16);
                    let func: SHChangeNotifyFn = std::mem::transmute(proc);
                    let wide: Vec<u16> = final_path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
                    func(0x00000002, 0x0005, wide.as_ptr(), std::ptr::null());
                }
            }
        }

        received_files.push(final_path);
    }

    let _ = socket.write_all(b"OK\n").await;
    on_files_received(received_files);
    Ok(())
}

pub struct FileTransferClient;

impl FileTransferClient {
    pub async fn send_files<P: AsRef<Path>>(
        target_addr: &str,
        security_token: &str,
        paths: &[P],
    ) -> Result<u64> {
        let mut valid_paths = Vec::new();
        for p in paths {
            let path = p.as_ref();
            if path.is_file() {
                valid_paths.push(path.to_path_buf());
            }
        }

        if valid_paths.is_empty() {
            return Ok(0);
        }

        let mut stream = TcpStream::connect(target_addr).await?;

        stream.write_all(FILE_TRANSFER_MAGIC).await?;

        let token_bytes = security_token.as_bytes();
        let token_len = token_bytes.len() as u16;
        stream.write_all(&token_len.to_be_bytes()).await?;
        stream.write_all(token_bytes).await?;

        let count = valid_paths.len() as u32;
        stream.write_all(&count.to_be_bytes()).await?;

        let mut total_bytes = 0u64;

        for path in &valid_paths {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
            let name_bytes = name.as_bytes();
            let name_len = name_bytes.len() as u16;
            stream.write_all(&name_len.to_be_bytes()).await?;
            stream.write_all(name_bytes).await?;

            let metadata = tokio::fs::metadata(path).await?;
            let size = metadata.len();
            stream.write_all(&size.to_be_bytes()).await?;

            let mut file = tokio::fs::File::open(path).await?;

            let mut buffer = vec![0u8; CHUNK_SIZE];
            let mut sent = 0u64;
            while sent < size {
                let to_read = std::cmp::min((size - sent) as usize, CHUNK_SIZE);
                file.read_exact(&mut buffer[..to_read]).await?;
                stream.write_all(&buffer[..to_read]).await?;
                sent += to_read as u64;
            }

            total_bytes += size;
        }

        stream.flush().await?;

        let mut ack = [0u8; 3];
        let _ = stream.read_exact(&mut ack).await;

        Ok(total_bytes)
    }
}
