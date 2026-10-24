use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    InvalidateRect, RoundRect, SelectObject, SetBkMode, SetTextColor, HDC, PAINTSTRUCT,
    DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, PS_SOLID, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    KillTimer, PostMessageW, PostThreadMessageW, RegisterClassW, SetLayeredWindowAttributes,
    SetTimer, SetWindowPos, ShowWindow, TranslateMessage, CS_DROPSHADOW, HTTRANSPARENT,
    HWND_TOPMOST, LWA_COLORKEY, MSG, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNA, WM_APP, WM_ERASEBKGND, WM_NCHITTEST, WM_PAINT,
    WM_TIMER, WM_USER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

const WM_BADGE_SHOW: u32 = WM_USER + 201;
const WM_BADGE_MOVE: u32 = WM_USER + 202;
const WM_BADGE_HIDE: u32 = WM_USER + 203;
const WM_STOP_BADGE: u32 = WM_APP + 55;

static CURRENT_BADGE_TEXT: Mutex<String> = Mutex::new(String::new());

pub struct DragBadge {
    hwnd_val: Arc<Mutex<Option<usize>>>,
    thread_id: Arc<Mutex<Option<u32>>>,
    is_visible: Arc<AtomicBool>,
}

impl DragBadge {
    pub fn new() -> Self {
        let hwnd_val = Arc::new(Mutex::new(None));
        let thread_id = Arc::new(Mutex::new(None));
        let is_visible = Arc::new(AtomicBool::new(false));

        let hwnd_holder = hwnd_val.clone();
        let tid_holder = thread_id.clone();

        std::thread::Builder::new()
            .name("nerve-drag-badge".into())
            .spawn(move || unsafe {
                let tid = windows::Win32::System::Threading::GetCurrentThreadId();
                if let Ok(mut guard) = tid_holder.lock() {
                    *guard = Some(tid);
                }

                let class_name = windows::core::w!("NerveDeskDragBadgeClass");
                let wnd_class = WNDCLASSW {
                    lpfnWndProc: Some(badge_wnd_proc),
                    hInstance: HINSTANCE::default(),
                    lpszClassName: class_name,
                    style: CS_DROPSHADOW,
                    ..Default::default()
                };
                let _ = RegisterClassW(&wnd_class);

                let hwnd = match CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                    class_name,
                    windows::core::w!("NerveDesk Drag Badge"),
                    WS_POPUP,
                    -1000,
                    -1000,
                    190,
                    38,
                    HWND::default(),
                    None,
                    HINSTANCE::default(),
                    None,
                ) {
                    Ok(h) => h,
                    Err(_) => return,
                };

                if let Ok(mut guard) = hwnd_holder.lock() {
                    *guard = Some(hwnd.0 as usize);
                }
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0x00FF00FF), 255, LWA_COLORKEY);

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                    if msg.message == WM_STOP_BADGE {
                        break;
                    }
                    if msg.message == WM_BADGE_SHOW {
                        let x = msg.wParam.0 as i32;
                        let y = msg.lParam.0 as i32;
                        let _ = SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            x + 16,
                            y + 16,
                            190,
                            38,
                            SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                        let _ = ShowWindow(hwnd, SW_SHOWNA);
                        let _ = InvalidateRect(hwnd, None, false);
                        let _ = SetTimer(hwnd, 1, 15, None);
                    } else if msg.message == WM_BADGE_MOVE {
                        let x = msg.wParam.0 as i32;
                        let y = msg.lParam.0 as i32;
                        let _ = SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            x + 16,
                            y + 16,
                            190,
                            38,
                            SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                        let _ = InvalidateRect(hwnd, None, false);
                    } else if msg.message == WM_BADGE_HIDE {
                        let _ = KillTimer(hwnd, 1);
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                let _ = DestroyWindow(hwnd);
            })
            .expect("Failed to spawn drag badge thread");

        Self {
            hwnd_val,
            thread_id,
            is_visible,
        }
    }

    pub fn show(&self, x: i32, y: i32, file_name: &str) {
        if let Ok(mut guard) = CURRENT_BADGE_TEXT.lock() {
            *guard = if file_name.is_empty() || file_name == "request" {
                "File".to_string()
            } else {
                file_name.to_string()
            };
        }
        self.is_visible.store(true, Ordering::SeqCst);
        if let Ok(guard) = self.hwnd_val.lock() {
            if let Some(h) = *guard {
                unsafe {
                    let hwnd = HWND(h as *mut std::ffi::c_void);
                    let _ =
                        PostMessageW(hwnd, WM_BADGE_SHOW, WPARAM(x as usize), LPARAM(y as isize));
                }
            }
        }
    }

    pub fn update_position(&self, x: i32, y: i32) {
        if !self.is_visible.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(guard) = self.hwnd_val.lock() {
            if let Some(h) = *guard {
                unsafe {
                    let hwnd = HWND(h as *mut std::ffi::c_void);
                    let _ =
                        PostMessageW(hwnd, WM_BADGE_MOVE, WPARAM(x as usize), LPARAM(y as isize));
                }
            }
        }
    }

    pub fn hide(&self) {
        self.is_visible.store(false, Ordering::SeqCst);
        if let Ok(guard) = self.hwnd_val.lock() {
            if let Some(h) = *guard {
                unsafe {
                    let hwnd = HWND(h as *mut std::ffi::c_void);
                    let _ = PostMessageW(hwnd, WM_BADGE_HIDE, WPARAM(0), LPARAM(0));
                }
            }
        }
    }

    pub fn is_active(&self) -> bool {
        self.is_visible.load(Ordering::Relaxed)
    }
}

impl Drop for DragBadge {
    fn drop(&mut self) {
        if let Ok(guard) = self.thread_id.lock() {
            if let Some(tid) = *guard {
                unsafe {
                    let _ = PostThreadMessageW(tid, WM_STOP_BADGE, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
}

unsafe extern "system" fn badge_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_TIMER => {
            let mut pt = windows::Win32::Foundation::POINT::default();
            if GetCursorPos(&mut pt).is_ok() {
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    pt.x + 16,
                    pt.y + 16,
                    190,
                    38,
                    SWP_NOACTIVATE,
                );
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !hdc.is_invalid() {
                paint_badge_content(hdc);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn paint_badge_content(hdc: HDC) {
    let rect = RECT { left: 0, top: 0, right: 190, bottom: 38 };
    let transparent = CreateSolidBrush(COLORREF(0x00FF00FF));
    let _ = FillRect(hdc, &rect, transparent);
    let _ = DeleteObject(transparent);

    let brush = CreateSolidBrush(COLORREF(0x001B1818));
    let pen = CreatePen(PS_SOLID, 2, COLORREF(0x00F6823B));
    let old_brush = SelectObject(hdc, brush);
    let old_pen = SelectObject(hdc, pen);
    let _ = RoundRect(hdc, 0, 0, 190, 38, 12, 12);
    let _ = SelectObject(hdc, old_brush);
    let _ = SelectObject(hdc, old_pen);
    let _ = DeleteObject(brush);
    let _ = DeleteObject(pen);

    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, COLORREF(0x00FAF5F5));
    let name = CURRENT_BADGE_TEXT.lock().map(|name| name.clone()).unwrap_or_else(|_| "File".into());
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    let mut text_rect = RECT { left: 10, top: 0, right: 180, bottom: 38 };
    let _ = DrawTextW(
        hdc,
        &mut wide,
        &mut text_rect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
}
