use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use crossbeam_channel::Sender;
use tracing::{info, trace};

use crate::format::ClipboardPayload;
use common::{NerveError, Result};

use windows::core::s;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    IsClipboardFormatAvailable, OpenClipboard, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExA, DefWindowProcA, DestroyWindow, DispatchMessageA, GetMessageA,
    PostMessageA, RegisterClassA, UnregisterClassA, MSG, WNDCLASSA, WS_OVERLAPPED,
    WM_CLIPBOARDUPDATE, WM_USER,
};

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const WM_STOP_CLIPBOARD: u32 = WM_USER + 101;

#[repr(C)]
struct DropFilesHeader {
    p_files: u32,
    pt_x: i32,
    pt_y: i32,
    f_nc: i32,
    f_wide: i32,
}

pub struct ClipboardListener {
    thread_handle: Option<JoinHandle<()>>,
    hwnd_val: usize,
    _last_seen_hash: Arc<AtomicU64>,
}

impl ClipboardListener {
    pub fn start(
        sender: Sender<ClipboardPayload>,
        last_seen_hash: Arc<AtomicU64>,
    ) -> Result<Self> {
        let (init_tx, init_rx) = crossbeam_channel::bounded::<Result<usize>>(1);
        let hash_clone = last_seen_hash.clone();

        let handle = std::thread::Builder::new()
            .name("nerve-clipboard-listener".into())
            .spawn(move || unsafe {
                let class_name = s!("NerveDeskClipboardClass");
                let wnd_class = WNDCLASSA {
                    lpfnWndProc: Some(clipboard_wnd_proc),
                    hInstance: HINSTANCE::default().into(),
                    lpszClassName: class_name,
                    ..Default::default()
                };

                if RegisterClassA(&wnd_class) == 0 {
                    let _ = init_tx.send(Err(NerveError::Platform(
                        "Failed to register clipboard window class".into(),
                    )));
                    return;
                }

                let hwnd = match CreateWindowExA(
                    windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                    class_name,
                    s!("NerveDeskClipboardWindow"),
                    WS_OVERLAPPED,
                    0, 0, 0, 0,
                    HWND::default(),
                    None,
                    HINSTANCE::default(),
                    None,
                ) {
                    Ok(h) => h,
                    Err(e) => {
                        let _ = init_tx.send(Err(NerveError::Platform(format!(
                            "Failed to create clipboard window: {e}"
                        ))));
                        return;
                    }
                };

                if let Err(e) = AddClipboardFormatListener(hwnd) {
                    let _ = DestroyWindow(hwnd);
                    let _ = init_tx.send(Err(NerveError::Platform(format!(
                        "AddClipboardFormatListener failed: {e}"
                    ))));
                    return;
                }

                let _ = init_tx.send(Ok(hwnd.0 as usize));
                info!("Clipboard listener active and registered with Windows");

                let mut msg = MSG::default();
                while GetMessageA(&mut msg, HWND::default(), 0, 0).as_bool() {
                    if msg.message == WM_STOP_CLIPBOARD {
                        break;
                    }
                    if msg.message == WM_CLIPBOARDUPDATE {
                        if let Some(payload) = read_clipboard_content() {
                            let hash = payload.compute_hash();
                            if hash_clone.swap(hash, Ordering::SeqCst) != hash {
                                trace!("Local clipboard changed, dispatching update (hash={:x})", hash);
                                let _ = sender.try_send(payload);
                            }
                        }
                    }
                    DispatchMessageA(&msg);
                }

                let _ = RemoveClipboardFormatListener(hwnd);
                let _ = DestroyWindow(hwnd);
                let _ = UnregisterClassA(class_name, HINSTANCE::default());
                info!("Clipboard listener thread safely stopped");
            })
            .map_err(|e| NerveError::Platform(format!("Failed to spawn clipboard thread: {e}")))?;

        let hwnd_val = init_rx
            .recv()
            .map_err(|_| NerveError::Platform("Clipboard thread died during init".into()))??;

        Ok(Self {
            thread_handle: Some(handle),
            hwnd_val,
            _last_seen_hash: last_seen_hash,
        })
    }

    pub fn stop(&mut self) {
        if let Some(handle) = self.thread_handle.take() {
            unsafe {
                let hwnd = HWND(self.hwnd_val as *mut std::ffi::c_void);
                let _ = PostMessageA(hwnd, WM_STOP_CLIPBOARD, WPARAM(0), LPARAM(0));
            }
            let _ = handle.join();
        }
    }
}

impl Drop for ClipboardListener {
    fn drop(&mut self) {
        self.stop();
    }
}

unsafe extern "system" fn clipboard_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcA(hwnd, msg, wparam, lparam)
}

fn read_clipboard_content() -> Option<ClipboardPayload> {
    unsafe {
        let mut opened = false;
        for _ in 0..10 {
            if OpenClipboard(HWND::default()).is_ok() {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        if !opened {
            return None;
        }

        if IsClipboardFormatAvailable(CF_HDROP).is_ok() {
            if let Ok(handle) = GetClipboardData(CF_HDROP) {
                let ptr = GlobalLock(HGLOBAL(handle.0));
                if !ptr.is_null() {
                    let header = &*(ptr as *const DropFilesHeader);
                    let mut files = Vec::new();
                    if header.f_wide != 0 {
                        let mut p = (ptr as *const u8).add(header.p_files as usize) as *const u16;
                        while *p != 0 {
                            let mut len = 0;
                            let start = p;
                            while *p != 0 {
                                len += 1;
                                p = p.add(1);
                            }
                            let slice = std::slice::from_raw_parts(start, len);
                            files.push(String::from_utf16_lossy(slice));
                            p = p.add(1);
                        }
                    }
                    let _ = GlobalUnlock(HGLOBAL(handle.0));
                    let _ = CloseClipboard();
                    if !files.is_empty() {
                        return Some(ClipboardPayload::Files(files));
                    }
                    return None;
                }
            }
        }

        let handle = match GetClipboardData(CF_UNICODETEXT) {
            Ok(h) => h,
            Err(_) => {
                let _ = CloseClipboard();
                return None;
            }
        };

        let ptr = GlobalLock(HGLOBAL(handle.0));
        if ptr.is_null() {
            let _ = CloseClipboard();
            return None;
        }

        let mut len = 0;
        let mut p = ptr as *const u16;
        while *p != 0 {
            len += 1;
            p = p.add(1);
        }
        let wide_slice = std::slice::from_raw_parts(ptr as *const u16, len);
        let text = String::from_utf16_lossy(wide_slice);

        let _ = GlobalUnlock(HGLOBAL(handle.0));
        let _ = CloseClipboard();

        Some(ClipboardPayload::Text(text))
    }
}

pub struct ClipboardWriter;

impl ClipboardWriter {
    pub fn write_text(text: &str) -> Result<()> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes_len = wide.len() * std::mem::size_of::<u16>();

        unsafe {
            let mut opened = false;
            for _ in 0..10 {
                if OpenClipboard(HWND::default()).is_ok() {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            if !opened {
                return Err(NerveError::Platform("Failed to open clipboard for writing".into()));
            }

            let _ = EmptyClipboard();

            let h_global = match GlobalAlloc(GMEM_MOVEABLE, bytes_len) {
                Ok(h) => h,
                Err(e) => {
                    let _ = CloseClipboard();
                    return Err(NerveError::Platform(format!("GlobalAlloc failed: {e}")));
                }
            };

            let ptr = GlobalLock(h_global);
            if ptr.is_null() {
                let _ = CloseClipboard();
                return Err(NerveError::Platform("GlobalLock failed".into()));
            }

            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr as *mut u8, bytes_len);
            let _ = GlobalUnlock(h_global);

            if let Err(e) = SetClipboardData(CF_UNICODETEXT, HANDLE(h_global.0)) {
                let _ = CloseClipboard();
                return Err(NerveError::Platform(format!("SetClipboardData failed: {e}")));
            }

            let _ = CloseClipboard();
            Ok(())
        }
    }

    pub fn write_files<P: AsRef<std::path::Path>>(paths: &[P]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }

        let mut wide_data: Vec<u16> = Vec::new();
        for path in paths {
            let s = path.as_ref().to_string_lossy();
            wide_data.extend(s.encode_utf16());
            wide_data.push(0);
        }
        wide_data.push(0);

        let header_size = std::mem::size_of::<DropFilesHeader>();
        let wide_bytes_len = wide_data.len() * std::mem::size_of::<u16>();
        let total_bytes = header_size + wide_bytes_len;

        let header = DropFilesHeader {
            p_files: header_size as u32,
            pt_x: 0,
            pt_y: 0,
            f_nc: 0,
            f_wide: 1,
        };

        unsafe {
            let mut opened = false;
            for _ in 0..10 {
                if OpenClipboard(HWND::default()).is_ok() {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            if !opened {
                return Err(NerveError::Platform("Failed to open clipboard for writing".into()));
            }

            let _ = EmptyClipboard();

            let h_global = match GlobalAlloc(GMEM_MOVEABLE, total_bytes) {
                Ok(h) => h,
                Err(e) => {
                    let _ = CloseClipboard();
                    return Err(NerveError::Platform(format!("GlobalAlloc failed: {e}")));
                }
            };

            let ptr = GlobalLock(h_global);
            if ptr.is_null() {
                let _ = CloseClipboard();
                return Err(NerveError::Platform("GlobalLock failed".into()));
            }

            std::ptr::copy_nonoverlapping(
                &header as *const DropFilesHeader as *const u8,
                ptr as *mut u8,
                header_size,
            );
            std::ptr::copy_nonoverlapping(
                wide_data.as_ptr() as *const u8,
                (ptr as *mut u8).add(header_size),
                wide_bytes_len,
            );
            let _ = GlobalUnlock(h_global);

            if let Err(e) = SetClipboardData(CF_HDROP, HANDLE(h_global.0)) {
                let _ = CloseClipboard();
                return Err(NerveError::Platform(format!("SetClipboardData failed: {e}")));
            }

            let _ = CloseClipboard();
            Ok(())
        }
    }
}
