use common::{NerveError, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info, warn};

pub const FILE_TRANSFER_PORT: u16 = 48106;
const FILE_TRANSFER_MAGIC: &[u8; 4] = b"NRV2";
const CHUNK_SIZE: usize = 262144; // 256 KB chunks for high throughput over LAN/Wi-Fi
const MAX_TRANSFER_FILE_SIZE: u64 = 16 * 1024 * 1024 * 1024;
const MAX_SECURE_RECORD: usize = 1024 * 1024 + 28; // Accept records up to 1 MB for backwards/forwards compatibility

fn tune_socket(socket: &TcpStream) {
    let _ = socket.set_nodelay(true);
    let sock_ref = socket2::SockRef::from(socket);
    let _ = sock_ref.set_send_buffer_size(1024 * 1024);
    let _ = sock_ref.set_recv_buffer_size(1024 * 1024);
}

static STAGED_FILES: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static PENDING_DROP_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn get_staging_dir() -> PathBuf {
    std::env::temp_dir().join("NerveDesk").join("staged")
}

pub fn get_default_transfer_dir() -> PathBuf {
    if let Some(desktop) = get_desktop_dir() {
        return desktop;
    }
    for variable in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
        if let Some(root) = std::env::var_os(variable) {
            let desktop = PathBuf::from(root).join("Desktop");
            if desktop.is_dir() {
                return desktop;
            }
        }
    }
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

fn get_desktop_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Com::CoTaskMemFree;
        use windows::Win32::UI::Shell::{
            FOLDERID_Desktop, SHGetFolderPathW, SHGetKnownFolderPath, CSIDL_DESKTOPDIRECTORY,
            KNOWN_FOLDER_FLAG,
        };
        if let Ok(path) =
            SHGetKnownFolderPath(&FOLDERID_Desktop, KNOWN_FOLDER_FLAG(0), HANDLE::default())
        {
            let value = path.to_string().ok();
            CoTaskMemFree(Some(path.0.cast()));
            if let Some(directory) = value.map(PathBuf::from).filter(|p| p.is_dir()) {
                return Some(directory);
            }
        }
        let mut legacy_path = [0u16; 260];
        if SHGetFolderPathW(
            None,
            CSIDL_DESKTOPDIRECTORY as i32,
            None,
            0,
            &mut legacy_path,
        )
        .is_ok()
        {
            let length = legacy_path
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(legacy_path.len());
            let directory = PathBuf::from(String::from_utf16_lossy(&legacy_path[..length]));
            return directory.is_dir().then_some(directory);
        }
        return None;
    }
    #[allow(unreachable_code)]
    None
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
        use windows::core::{Interface, VARIANT};
        use windows::Win32::Foundation::POINT;
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::UI::Shell::{IShellWindows, IWebBrowser2, ShellWindows};
        use windows::Win32::UI::WindowsAndMessaging::{
            GetAncestor, GetClassNameW, GetCursorPos, WindowFromPoint, GA_ROOT,
        };

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
                    if let Some(desktop) = get_desktop_dir() {
                        return desktop;
                    }
                } else if class_name == "CabinetWClass" || class_name == "ExploreWClass" {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                    if let Ok(sw) =
                        CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_ALL)
                    {
                        if let Ok(count) = sw.Count() {
                            for i in 0..count {
                                let v = VARIANT::from(i);
                                if let Ok(item) = sw.Item(&v) {
                                    if let Ok(wb) = item.cast::<IWebBrowser2>() {
                                        if let Ok(whwnd) = wb.HWND() {
                                            if whwnd.0 as isize == target_hwnd.0 as isize {
                                                if let Ok(url) = wb.LocationURL() {
                                                    if let Some(path) =
                                                        url_to_path(&url.to_string())
                                                    {
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

pub fn get_dragged_files(source_window: isize) -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::core::{Interface, VARIANT};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
        };
        use windows::Win32::UI::Shell::{
            IShellFolderViewDual, IShellWindows, IWebBrowser2, ShellWindows, SWC_DESKTOP,
            SWFO_NEEDDISPATCH,
        };
        use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;

        if source_window == 0 {
            return Vec::new();
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        if let Ok(sw) = CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_ALL) {
            if let Ok(count) = sw.Count() {
                for i in 0..count {
                    let v = VARIANT::from(i);
                    if let Ok(item) = sw.Item(&v) {
                        if let Ok(wb) = item.cast::<IWebBrowser2>() {
                            let is_target =
                                wb.HWND().is_ok_and(|hwnd| hwnd.0 as isize == source_window);

                            if !is_target {
                                continue;
                            }

                            if let Ok(doc) = wb.Document() {
                                if let Ok(view) = doc.cast::<IShellFolderViewDual>() {
                                    let files = selected_files_from_view(&view);
                                    if !files.is_empty() {
                                        return files;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut class_buf = [0u16; 256];
        let len = GetClassNameW(HWND(source_window as *mut _), &mut class_buf);
        let class_name = String::from_utf16_lossy(&class_buf[..len as usize]);
        if class_name == "Progman" || class_name == "WorkerW" {
            if let Ok(sw) = CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_ALL) {
                let mut desktop_hwnd = 0;
                if let Ok(dispatch) = sw.FindWindowSW(
                    std::ptr::null(),
                    std::ptr::null(),
                    SWC_DESKTOP,
                    &mut desktop_hwnd,
                    SWFO_NEEDDISPATCH,
                ) {
                    if let Ok(view) = dispatch.cast::<IShellFolderViewDual>() {
                        let files = selected_files_from_view(&view);
                        if !files.is_empty() {
                            return files;
                        }
                    }
                    if let Ok(browser) = dispatch.cast::<IWebBrowser2>() {
                        if let Ok(doc) = browser.Document() {
                            if let Ok(view) = doc.cast::<IShellFolderViewDual>() {
                                return selected_files_from_view(&view);
                            }
                        }
                    }
                }
            }
        }

        Vec::new()
    }
}

#[cfg(target_os = "windows")]
unsafe fn selected_files_from_view(
    view: &windows::Win32::UI::Shell::IShellFolderViewDual,
) -> Vec<PathBuf> {
    use windows::core::VARIANT;
    let mut files = Vec::new();
    if let Ok(items) = view.SelectedItems() {
        if let Ok(count) = items.Count() {
            for index in 0..count {
                if let Ok(item) = items.Item(&VARIANT::from(index)) {
                    if let Ok(path) = item.Path() {
                        let path = PathBuf::from(path.to_string());
                        if path.is_file() {
                            files.push(path);
                        }
                    }
                }
            }
        }
    }
    files
}

pub fn notify_shell_update(file_path: &Path, dir_path: &Path) {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::core::s;
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
        if let Ok(shell32) = LoadLibraryA(s!("shell32.dll")) {
            if let Some(proc) = GetProcAddress(shell32, s!("SHChangeNotify")) {
                type SHChangeNotifyFn = unsafe extern "system" fn(i32, u32, *const u16, *const u16);
                let func: SHChangeNotifyFn = std::mem::transmute(proc);
                let wide_file: Vec<u16> = file_path
                    .to_string_lossy()
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                let wide_dir: Vec<u16> = dir_path
                    .to_string_lossy()
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                func(0x00000002, 0x0005, wide_file.as_ptr(), std::ptr::null());
                func(0x00001000, 0x0005, wide_dir.as_ptr(), std::ptr::null());
            }
        }
    }
}

pub fn commit_file_drop(target_dir: PathBuf) -> Vec<PathBuf> {
    let _ = std::fs::create_dir_all(&target_dir);
    let mut files_to_move = Vec::new();
    if let Ok(mut guard) = STAGED_FILES.lock() {
        files_to_move = guard.drain(..).collect();
    }

    if files_to_move.is_empty() {
        if let Ok(mut guard) = PENDING_DROP_DIR.lock() {
            *guard = Some(target_dir);
        }
        return Vec::new();
    }

    let mut moved = Vec::new();
    for src in files_to_move {
        if let Some(fname) = src.file_name().and_then(|n| n.to_str()) {
            let dest = get_unique_path(&target_dir, fname);
            if std::fs::rename(&src, &dest).is_ok()
                || std::fs::copy(&src, &dest).map(|_| ()).is_ok()
            {
                let _ = std::fs::remove_file(&src);
                notify_shell_update(&dest, &target_dir);
                moved.push(dest);
            }
        }
    }
    moved
}

pub fn clear_file_drag() {
    if let Ok(mut guard) = PENDING_DROP_DIR.lock() {
        *guard = None;
    }
    if let Ok(mut guard) = STAGED_FILES.lock() {
        for f in guard.drain(..) {
            let _ = std::fs::remove_file(f);
        }
    }
    let staging = get_staging_dir();
    let _ = std::fs::remove_dir_all(&staging);
}

fn get_unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
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

fn record_context(sequence: u64) -> [u8; 21] {
    let mut context = [0u8; 21];
    context[..13].copy_from_slice(b"file-transfer");
    context[13..].copy_from_slice(&sequence.to_be_bytes());
    context
}

async fn write_secure_record<S: AsyncWrite + Unpin>(
    socket: &mut S,
    key: &str,
    sequence: &mut u64,
    plaintext: &[u8],
) -> Result<()> {
    let encrypted =
        crate::secure_input::seal_payload(key, &record_context(*sequence), plaintext, CHUNK_SIZE)
            .map_err(NerveError::Protocol)?;
    let mut packet = Vec::with_capacity(4 + encrypted.len());
    packet.extend_from_slice(&(encrypted.len() as u32).to_be_bytes());
    packet.extend_from_slice(&encrypted);
    socket.write_all(&packet).await?;
    *sequence = sequence
        .checked_add(1)
        .ok_or_else(|| NerveError::Protocol("File transfer sequence overflow".into()))?;
    Ok(())
}

async fn read_secure_record<S: AsyncRead + Unpin>(
    socket: &mut S,
    key: &str,
    sequence: &mut u64,
    replay: &mut crate::secure_input::ReplayGuard,
) -> Result<Vec<u8>> {
    let mut length = [0u8; 4];
    socket.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if !(28..=MAX_SECURE_RECORD).contains(&length) {
        return Err(NerveError::Protocol(
            "Invalid encrypted file record length".into(),
        ));
    }
    let mut encrypted = vec![0u8; length];
    socket.read_exact(&mut encrypted).await?;
    let plaintext = replay
        .open_payload(key, &record_context(*sequence), &encrypted, CHUNK_SIZE)
        .map_err(NerveError::Protocol)?;
    *sequence = sequence
        .checked_add(1)
        .ok_or_else(|| NerveError::Protocol("File transfer sequence overflow".into()))?;
    Ok(plaintext)
}

impl FileTransferServer {
    pub async fn start<F>(
        port: u16,
        security_key: Arc<Mutex<Option<String>>>,
        paired_peer_ip: Arc<Mutex<Option<String>>>,
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
                        tune_socket(&socket);
                        if !enabled.load(Ordering::Relaxed) {
                            continue;
                        }
                        let paired = paired_peer_ip.lock().ok().and_then(|ip| ip.clone());
                        if paired.as_deref() != Some(addr.ip().to_string().as_str()) {
                            continue;
                        }
                        info!("Incoming file transfer connection from {addr}");
                        let key = security_key.clone();
                        let enabled_for_connection = enabled.clone();
                        let cb = on_files_received.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                handle_file_connection(socket, key, enabled_for_connection, cb)
                                    .await
                            {
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
    security_key: Arc<Mutex<Option<String>>>,
    enabled: Arc<AtomicBool>,
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

    let key = security_key
        .lock()
        .ok()
        .and_then(|key| key.clone())
        .ok_or_else(|| NerveError::Protocol("File transfer is not paired".into()))?;
    let mut replay = crate::secure_input::ReplayGuard::new();
    let mut received_sequence = 0u64;

    let count_buf =
        read_secure_record(&mut socket, &key, &mut received_sequence, &mut replay).await?;
    if count_buf.len() != 4 {
        return Err(NerveError::Protocol("Invalid encrypted file count".into()));
    }
    let file_count = u32::from_be_bytes(count_buf.try_into().unwrap()) as usize;
    if file_count == 0 || file_count > 1000 {
        return Err(NerveError::Protocol("Invalid file count".into()));
    }

    let staging_dir = get_staging_dir();
    let _ = std::fs::create_dir_all(&staging_dir);
    let mut received_files = Vec::new();

    for _ in 0..file_count {
        ensure_transfer_allowed(&security_key, &key, &enabled)?;
        let metadata =
            read_secure_record(&mut socket, &key, &mut received_sequence, &mut replay).await?;
        if metadata.len() < 10 {
            return Err(NerveError::Protocol(
                "Invalid encrypted file metadata".into(),
            ));
        }
        let name_len = u16::from_be_bytes(metadata[..2].try_into().unwrap()) as usize;
        if name_len == 0 || name_len > 1024 || metadata.len() != 2 + name_len + 8 {
            return Err(NerveError::Protocol("Invalid filename length".into()));
        }
        let raw_name = String::from_utf8_lossy(&metadata[2..2 + name_len]);
        let safe_name = Path::new(&*raw_name)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("received_file");

        let file_size = u64::from_be_bytes(metadata[2 + name_len..].try_into().unwrap());
        if file_size > MAX_TRANSFER_FILE_SIZE {
            return Err(NerveError::Protocol("File exceeds transfer limit".into()));
        }

        let final_path = get_unique_path(&staging_dir, safe_name);
        let final_name = final_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(safe_name);
        let part_path = staging_dir.join(format!("{final_name}.part"));

        let out_file = tokio::fs::File::create(&part_path).await?;
        let mut out_file = BufWriter::with_capacity(CHUNK_SIZE * 2, out_file);

        let mut remaining = file_size;
        while remaining > 0 {
            ensure_transfer_allowed(&security_key, &key, &enabled)?;
            let to_read = std::cmp::min(remaining as usize, CHUNK_SIZE);
            let chunk =
                read_secure_record(&mut socket, &key, &mut received_sequence, &mut replay).await?;
            if chunk.len() != to_read {
                return Err(NerveError::Protocol("Invalid encrypted file chunk".into()));
            }
            out_file.write_all(&chunk).await?;
            remaining -= to_read as u64;
        }

        out_file.flush().await?;
        drop(out_file);

        let _ = tokio::fs::remove_file(&final_path).await;
        tokio::fs::rename(&part_path, &final_path).await?;

        received_files.push(final_path);
    }

    ensure_transfer_allowed(&security_key, &key, &enabled)?;

    let mut pending_target = None;
    if let Ok(mut guard) = PENDING_DROP_DIR.lock() {
        pending_target = guard.take();
    }
    let save_immediately = pending_target.is_some();

    let mut final_files = Vec::new();
    if let Some(target_dir) = pending_target {
        let _ = std::fs::create_dir_all(&target_dir);
        for src in received_files {
            if let Some(fname) = src.file_name().and_then(|n| n.to_str()) {
                let dest = get_unique_path(&target_dir, fname);
                if std::fs::rename(&src, &dest).is_ok()
                    || std::fs::copy(&src, &dest).map(|_| ()).is_ok()
                {
                    let _ = std::fs::remove_file(&src);
                    notify_shell_update(&dest, &target_dir);
                    final_files.push(dest);
                }
            }
        }
        if final_files.len() != file_count {
            return Err(NerveError::Platform(
                "Could not save every dropped file".into(),
            ));
        }
    } else {
        if let Ok(mut guard) = STAGED_FILES.lock() {
            guard.extend(received_files.clone());
        }
        final_files = received_files;
    }

    if save_immediately {
        on_files_received(final_files);
    }
    let mut ack_sequence = 0u64;
    write_secure_record(&mut socket, &key, &mut ack_sequence, b"OK").await?;
    Ok(())
}

fn ensure_transfer_allowed(
    current_key: &Mutex<Option<String>>,
    transfer_key: &str,
    enabled: &AtomicBool,
) -> Result<()> {
    if !enabled.load(Ordering::SeqCst)
        || !current_key
            .lock()
            .is_ok_and(|key| key.as_deref() == Some(transfer_key))
    {
        return Err(NerveError::Protocol(
            "File transfer authorization was revoked".into(),
        ));
    }
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
        if valid_paths.len() > 1000 || !(16..=256).contains(&security_token.len()) {
            return Err(NerveError::Protocol(
                "Invalid transfer count or pairing key".into(),
            ));
        }

        let mut stream = TcpStream::connect(target_addr).await?;
        tune_socket(&stream);

        stream.write_all(FILE_TRANSFER_MAGIC).await?;
        let mut sent_sequence = 0u64;
        let count = valid_paths.len() as u32;
        write_secure_record(
            &mut stream,
            security_token,
            &mut sent_sequence,
            &count.to_be_bytes(),
        )
        .await?;

        let mut total_bytes = 0u64;

        for path in &valid_paths {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
            let name_bytes = name.as_bytes();
            if name_bytes.is_empty() || name_bytes.len() > 1024 {
                return Err(NerveError::Protocol("Invalid filename length".into()));
            }
            let name_len = name_bytes.len() as u16;

            let metadata = tokio::fs::metadata(path).await?;
            let size = metadata.len();
            if size > MAX_TRANSFER_FILE_SIZE {
                return Err(NerveError::Protocol("File exceeds transfer limit".into()));
            }
            let mut file_header = Vec::with_capacity(name_bytes.len() + 10);
            file_header.extend_from_slice(&name_len.to_be_bytes());
            file_header.extend_from_slice(name_bytes);
            file_header.extend_from_slice(&size.to_be_bytes());
            write_secure_record(
                &mut stream,
                security_token,
                &mut sent_sequence,
                &file_header,
            )
            .await?;

            let file = tokio::fs::File::open(path).await?;
            let mut file = BufReader::with_capacity(CHUNK_SIZE * 2, file);

            let mut buffer = vec![0u8; CHUNK_SIZE];
            let mut sent = 0u64;
            while sent < size {
                let to_read = std::cmp::min((size - sent) as usize, CHUNK_SIZE);
                file.read_exact(&mut buffer[..to_read]).await?;
                write_secure_record(
                    &mut stream,
                    security_token,
                    &mut sent_sequence,
                    &buffer[..to_read],
                )
                .await?;
                sent += to_read as u64;
            }

            total_bytes += size;
        }

        stream.flush().await?;

        let mut replay = crate::secure_input::ReplayGuard::new();
        let mut ack_sequence = 0u64;
        let ack =
            read_secure_record(&mut stream, security_token, &mut ack_sequence, &mut replay).await?;
        if ack != b"OK" {
            return Err(NerveError::Protocol(
                "File transfer was not acknowledged".into(),
            ));
        }

        Ok(total_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn secure_records_require_matching_key_and_sequence() {
        let (mut sender, mut receiver) = tokio::io::duplex(1024);
        let key = "1234567890abcdef";
        let mut send_sequence = 0;
        let mut receive_sequence = 0;
        let mut replay = crate::secure_input::ReplayGuard::new();

        write_secure_record(&mut sender, key, &mut send_sequence, b"file contents")
            .await
            .unwrap();
        assert!(read_secure_record(
            &mut receiver,
            "fedcba0987654321",
            &mut receive_sequence,
            &mut replay,
        )
        .await
        .is_err());
        assert_eq!(receive_sequence, 0);

        write_secure_record(&mut sender, key, &mut send_sequence, b"next")
            .await
            .unwrap();
        assert!(
            read_secure_record(&mut receiver, key, &mut receive_sequence, &mut replay)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn secure_records_round_trip() {
        let (mut sender, mut receiver) = tokio::io::duplex(1024);
        let key = "1234567890abcdef";
        let mut send_sequence = 0;
        let mut receive_sequence = 0;
        let mut replay = crate::secure_input::ReplayGuard::new();

        for contents in [b"metadata".as_slice(), b"file contents"] {
            write_secure_record(&mut sender, key, &mut send_sequence, contents)
                .await
                .unwrap();
            assert_eq!(
                read_secure_record(&mut receiver, key, &mut receive_sequence, &mut replay)
                    .await
                    .unwrap(),
                contents
            );
        }
    }

    #[tokio::test]
    async fn large_chunk_streaming_round_trip() {
        let (mut sender, mut receiver) = tokio::io::duplex(512 * 1024);
        let key = "1234567890abcdef";
        let mut send_seq = 0;
        let mut recv_seq = 0;
        let mut replay = crate::secure_input::ReplayGuard::new();
        let payload = vec![0x5Au8; 768 * 1024]; // 768 KB spanning multiple 256 KB chunks

        let sender_task = tokio::spawn(async move {
            for chunk in payload.chunks(CHUNK_SIZE) {
                write_secure_record(&mut sender, key, &mut send_seq, chunk)
                    .await
                    .unwrap();
            }
        });

        let mut received = Vec::new();
        while received.len() < 768 * 1024 {
            let chunk = read_secure_record(&mut receiver, key, &mut recv_seq, &mut replay)
                .await
                .unwrap();
            received.extend_from_slice(&chunk);
        }

        sender_task.await.unwrap();
        assert_eq!(received.len(), 768 * 1024);
        assert_eq!(received[0], 0x5A);
        assert_eq!(received[768 * 1024 - 1], 0x5A);
    }
}
