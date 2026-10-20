use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetDC, ReleaseDC, SetBrushOrgEx, SetStretchBltMode, StretchDIBits, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HALFTONE, RGBQUAD, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetSystemMetrics,
    LoadCursorW, PeekMessageW, RegisterClassW, SetCursor, SetWindowPos, ShowWindow,
    TranslateMessage, MSG, PM_REMOVE, SM_CXSCREEN, SM_CYSCREEN, SWP_HIDEWINDOW,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW, WM_DESTROY, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETCURSOR, WNDCLASSW, WS_EX_TOPMOST, WS_POPUP, HWND_TOPMOST,
    IDC_ARROW,
};

pub struct ProjectionViewer {
    frame_slot: Arc<Mutex<Option<(u32, u32, Vec<u8>)>>>,
    is_active: Arc<AtomicBool>,
    last_frame_time: Arc<Mutex<Instant>>,
}

impl ProjectionViewer {
    pub fn new() -> Self {
        let frame_slot: Arc<Mutex<Option<(u32, u32, Vec<u8>)>>> = Arc::new(Mutex::new(None));
        let is_active = Arc::new(AtomicBool::new(false));
        let last_frame_time = Arc::new(Mutex::new(Instant::now()));

        let slot_clone = frame_slot.clone();
        let active_clone = is_active.clone();
        let time_clone = last_frame_time.clone();

        std::thread::Builder::new()
            .name("nerve-projection-viewer".into())
            .spawn(move || unsafe {
                let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

                let class_name = windows::core::w!("NerveDeskProjectionWindowClass");
                let wnd_class = WNDCLASSW {
                    lpfnWndProc: Some(projection_wnd_proc),
                    hInstance: HINSTANCE::default(),
                    lpszClassName: class_name,
                    hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };

                let _ = RegisterClassW(&wnd_class);

                let sw = GetSystemMetrics(SM_CXSCREEN);
                let sh = GetSystemMetrics(SM_CYSCREEN);

                let hwnd = match CreateWindowExW(
                    WS_EX_TOPMOST,
                    class_name,
                    windows::core::w!("NerveDesk Projection Display"),
                    WS_POPUP,
                    0,
                    0,
                    sw,
                    sh,
                    HWND::default(),
                    None,
                    HINSTANCE::default(),
                    None,
                ) {
                    Ok(h) => h,
                    Err(_) => return,
                };

                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    sw,
                    sh,
                    SWP_HIDEWINDOW | SWP_NOACTIVATE,
                );
                let _ = ShowWindow(hwnd, SW_HIDE);

                let mut is_shown = false;

                loop {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                        if msg.message == WM_DESTROY {
                            return;
                        }
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }

                    let is_active = active_clone.load(Ordering::Relaxed);
                    let timed_out = if let Ok(t) = time_clone.lock() {
                        t.elapsed() >= Duration::from_millis(1500)
                    } else {
                        false
                    };
                    let active = is_active && !timed_out;

                    if active {
                        if !is_shown {
                            let sw = GetSystemMetrics(SM_CXSCREEN);
                            let sh = GetSystemMetrics(SM_CYSCREEN);

                            let _ = SetWindowPos(
                                hwnd,
                                HWND_TOPMOST,
                                0,
                                0,
                                sw,
                                sh,
                                SWP_SHOWWINDOW,
                            );
                            let _ = ShowWindow(hwnd, SW_SHOW);
                            is_shown = true;
                        }

                        let frame_opt = if let Ok(mut guard) = slot_clone.lock() {
                            guard.take()
                        } else {
                            None
                        };

                        if let Some((w, h, pixels)) = frame_opt {
                            if !pixels.is_empty() {
                                let mut client_rect = windows::Win32::Foundation::RECT::default();
                                let _ = GetClientRect(hwnd, &mut client_rect);
                                let client_w = client_rect.right - client_rect.left;
                                let client_h = client_rect.bottom - client_rect.top;

                                let bmi = BITMAPINFO {
                                    bmiHeader: BITMAPINFOHEADER {
                                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                                        biWidth: w as i32,
                                        biHeight: -(h as i32),
                                        biPlanes: 1,
                                        biBitCount: 32,
                                        biCompression: BI_RGB.0,
                                        biSizeImage: 0,
                                        biXPelsPerMeter: 0,
                                        biYPelsPerMeter: 0,
                                        biClrUsed: 0,
                                        biClrImportant: 0,
                                    },
                                    bmiColors: [RGBQUAD::default()],
                                };

                                let hdc = GetDC(hwnd);
                                if !hdc.is_invalid() {
                                    let _ = SetStretchBltMode(hdc, HALFTONE);
                                    let _ = SetBrushOrgEx(hdc, 0, 0, None);
                                    let _ = StretchDIBits(
                                        hdc,
                                        0,
                                        0,
                                        client_w,
                                        client_h,
                                        0,
                                        0,
                                        w as i32,
                                        h as i32,
                                        Some(pixels.as_ptr() as *const _),
                                        &bmi,
                                        DIB_RGB_COLORS,
                                        SRCCOPY,
                                    );
                                    let _ = ReleaseDC(hwnd, hdc);
                                }
                            }
                        }
                    } else if is_shown {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                        is_shown = false;
                    }

                    std::thread::sleep(std::time::Duration::from_millis(8));
                }
            })
            .expect("Failed to spawn projection viewer thread");

        Self {
            frame_slot,
            is_active,
            last_frame_time,
        }
    }

    pub fn present_frame(&self, width: u32, height: u32, bgra: Vec<u8>) {
        if let Ok(mut time_guard) = self.last_frame_time.lock() {
            *time_guard = Instant::now();
        }
        self.is_active.store(true, Ordering::Relaxed);
        if let Ok(mut guard) = self.frame_slot.lock() {
            *guard = Some((width, height, bgra));
        }
    }

    pub fn stop(&self) {
        self.is_active.store(false, Ordering::Relaxed);
        if let Ok(mut guard) = self.frame_slot.lock() {
            *guard = None;
        }
    }
}

unsafe extern "system" fn projection_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_KEYDOWN => {
            if wparam.0 == VK_ESCAPE.0 as usize {
                let _ = ShowWindow(hwnd, SW_HIDE);
                return LRESULT(0);
            }
        }
        WM_SETCURSOR => {
            let cursor = LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default();
            SetCursor(cursor);
            return LRESULT(1);
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN | WM_MBUTTONUP => {
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
