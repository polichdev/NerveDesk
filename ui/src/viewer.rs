use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{encode_packet, ButtonState, InputEvent, KeyState, MouseButton};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    GetDC, GetMonitorInfoW, MonitorFromWindow, PatBlt, ReleaseDC, ScreenToClient,
    SelectObject, SetStretchBltMode, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, BLACKNESS, COLORONCOLOR, DIB_RGB_COLORS, HDC, HBITMAP, HGDIOBJ,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, RGBQUAD, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect,
    GetSystemMetrics, LoadCursorW, PeekMessageW, RegisterClassW,
    SetCursor, SetWindowPos, ShowWindow, TranslateMessage,
    CS_DBLCLKS, CS_OWNDC, MSG, PM_REMOVE, SM_CXSCREEN, SM_CYSCREEN,
    SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SW_HIDE, SW_SHOW,
    WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SETCURSOR, WNDCLASSW, WS_EX_TOPMOST, WS_POPUP, HWND_NOTOPMOST, HWND_TOPMOST,
    IDC_ARROW,
};

const VK_F11_CODE: usize = 0x7A;

// ── Offscreen double-buffer ───────────────────────────────────────────────────
struct OffscreenBuffer {
    hdc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    width: i32,
    height: i32,
}

impl OffscreenBuffer {
    fn new(screen_hdc: HDC, w: i32, h: i32) -> Option<Self> {
        unsafe {
            let hdc = CreateCompatibleDC(screen_hdc);
            if hdc.is_invalid() { return None; }
            let bitmap = CreateCompatibleBitmap(screen_hdc, w, h);
            if bitmap.is_invalid() { let _ = DeleteDC(hdc); return None; }
            let old_bitmap = SelectObject(hdc, bitmap);
            Some(Self { hdc, bitmap, old_bitmap, width: w, height: h })
        }
    }

    fn ensure(&mut self, screen_hdc: HDC, w: i32, h: i32) {
        if self.width == w && self.height == h { return; }
        unsafe {
            let new_bm = CreateCompatibleBitmap(screen_hdc, w, h);
            if !new_bm.is_invalid() {
                SelectObject(self.hdc, new_bm);
                let _ = DeleteObject(self.bitmap);
                self.bitmap = new_bm;
                self.width = w;
                self.height = h;
            }
        }
    }
}

impl Drop for OffscreenBuffer {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.hdc, self.old_bitmap);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.hdc);
        }
    }
}

// ── Shared state between render thread and wndproc ────────────────────────────
#[derive(Clone, Copy)]
pub struct FrameMeta {
    pub width: u32,
    pub height: u32,
    pub offset_x: i16,
    pub offset_y: i16,
}

pub struct ViewerSharedState {
    /// Channel to send raw framed packets back to the PC (input events + ACK).
    pub input_channel: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>,
    pub current_meta: Mutex<Option<FrameMeta>>,
    /// Destination rect inside the window (letterbox result).
    pub dst_rect: Mutex<Option<(i32, i32, i32, i32)>>,
    pub is_fullscreen: AtomicBool,
}

impl ViewerSharedState {
    pub fn new() -> Self {
        Self {
            input_channel: Mutex::new(None),
            current_meta: Mutex::new(None),
            dst_rect: Mutex::new(None),
            is_fullscreen: AtomicBool::new(true),
        }
    }

    pub fn send_input_event(&self, event: &InputEvent) {
        let guard = self.input_channel.lock().unwrap();
        let Some(ref sender) = *guard else { return };

        let mut buf = [0u8; 1024];
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;
        if let Ok(len) = encode_packet(event, 1, ts, &mut buf) {
            let mut packet = Vec::with_capacity(3 + len);
            packet.push(0x02);
            packet.extend_from_slice(&(len as u16).to_le_bytes());
            packet.extend_from_slice(&buf[..len]);
            let _ = sender.send(packet);
        }
    }

    /// Map window-client coords to the PC's absolute screen coords.
    pub fn map_client_to_pc(&self, client_x: i32, client_y: i32) -> Option<(i32, i32)> {
        let (dst_x, dst_y, dst_w, dst_h) = (*self.dst_rect.lock().unwrap())?;
        let meta = (*self.current_meta.lock().unwrap())?;
        if dst_w <= 0 || dst_h <= 0 { return None; }
        if client_x < dst_x || client_x >= dst_x + dst_w
            || client_y < dst_y || client_y >= dst_y + dst_h
        { return None; }
        let nx = (client_x - dst_x) as f64 / dst_w as f64;
        let ny = (client_y - dst_y) as f64 / dst_h as f64;
        Some((
            meta.offset_x as i32 + (nx * meta.width  as f64).round() as i32,
            meta.offset_y as i32 + (ny * meta.height as f64).round() as i32,
        ))
    }
}

static VIEWER_STATE: OnceLock<Arc<ViewerSharedState>> = OnceLock::new();

// ── Frame slot ────────────────────────────────────────────────────────────────
pub struct FramePacket {
    pub width: u32,
    pub height: u32,
    pub offset_x: i16,
    pub offset_y: i16,
    pub bgra: Vec<u8>,
}

// ── Public handle ─────────────────────────────────────────────────────────────
pub struct ProjectionViewer {
    shared: Arc<ViewerSharedState>,
    frame_slot: Arc<Mutex<Option<FramePacket>>>,
    is_active: Arc<AtomicBool>,
    last_frame_time: Arc<Mutex<Instant>>,
}

impl ProjectionViewer {
    pub fn new() -> Self {
        let frame_slot: Arc<Mutex<Option<FramePacket>>> = Arc::new(Mutex::new(None));
        let is_active = Arc::new(AtomicBool::new(false));
        let last_frame_time = Arc::new(Mutex::new(Instant::now()));
        let shared = Arc::new(ViewerSharedState::new());
        let _ = VIEWER_STATE.set(shared.clone());

        let slot_clone       = frame_slot.clone();
        let active_clone     = is_active.clone();
        let time_clone       = last_frame_time.clone();
        let shared_clone     = shared.clone();

        std::thread::Builder::new()
            .name("nerve-projection-viewer".into())
            .spawn(move || unsafe { render_loop(slot_clone, active_clone, time_clone, shared_clone) })
            .expect("Failed to spawn projection viewer thread");

        Self { shared, frame_slot, is_active, last_frame_time }
    }

    pub fn set_input_channel(&self, ch: Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>) {
        *self.shared.input_channel.lock().unwrap() = ch;
    }

    pub fn present_frame(&self, width: u32, height: u32, offset_x: i16, offset_y: i16, bgra: Vec<u8>) {
        *self.last_frame_time.lock().unwrap() = Instant::now();
        self.is_active.store(true, Ordering::Relaxed);
        *self.frame_slot.lock().unwrap() = Some(FramePacket { width, height, offset_x, offset_y, bgra });
    }

    pub fn stop(&self) {
        self.is_active.store(false, Ordering::Relaxed);
        *self.frame_slot.lock().unwrap() = None;
        *self.shared.input_channel.lock().unwrap() = None;
    }
}

// ── Render thread ─────────────────────────────────────────────────────────────
unsafe fn render_loop(
    slot: Arc<Mutex<Option<FramePacket>>>,
    active: Arc<AtomicBool>,
    last_time: Arc<Mutex<Instant>>,
    shared: Arc<ViewerSharedState>,
) {
    let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

    let class_name = windows::core::w!("NerveDeskProjectionWindowClass");
    let wnd_class = WNDCLASSW {
        // CS_DBLCLKS enables WM_LBUTTONDBLCLK; CS_OWNDC keeps DC stable
        style: CS_DBLCLKS | CS_OWNDC,
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
        WS_EX_TOPMOST, class_name,
        windows::core::w!("NerveDesk Mirror (F11 = вікно/повний екран)"),
        WS_POPUP,
        0, 0, sw, sh,
        HWND::default(), None, HINSTANCE::default(), None,
    ) {
        Ok(h) => h,
        Err(_) => return,
    };

    // Start hidden
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, sw, sh, SWP_HIDEWINDOW | SWP_NOACTIVATE);
    let _ = ShowWindow(hwnd, SW_HIDE);

    let mut is_shown = false;
    let mut offscreen: Option<OffscreenBuffer> = None;

    loop {
        // Drain Win32 messages
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_DESTROY { return; }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let is_active  = active.load(Ordering::Relaxed);
        let timed_out  = last_time.lock().unwrap().elapsed() >= Duration::from_secs(30);
        let should_run = is_active && !timed_out;

        if should_run {
            if !is_shown {
                show_window_fullscreen_or_restore(hwnd, &shared, true);
                is_shown = true;
            }

            // Try to grab the latest frame
            let frame_opt = slot.lock().unwrap().take();
            if let Some(pkt) = frame_opt {
                let (w, h) = (pkt.width, pkt.height);
                if w > 0 && h > 0 && pkt.bgra.len() == (w as usize) * (h as usize) * 4 {
                    *shared.current_meta.lock().unwrap() = Some(FrameMeta {
                        width: w, height: h,
                        offset_x: pkt.offset_x, offset_y: pkt.offset_y,
                    });

                    let mut cr = RECT::default();
                    let _ = GetClientRect(hwnd, &mut cr);
                    let (cw, ch) = (cr.right - cr.left, cr.bottom - cr.top);

                    if cw > 0 && ch > 0 {
                        if let Some((dx, dy, dw, dh)) = fit_frame(w, h, cw, ch) {
                            *shared.dst_rect.lock().unwrap() = Some((dx, dy, dw, dh));

                            let bmi = BITMAPINFO {
                                bmiHeader: BITMAPINFOHEADER {
                                    biSize:     std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                                    biWidth:    w as i32,
                                    biHeight:   -(h as i32),
                                    biPlanes:   1,
                                    biBitCount: 32,
                                    biCompression: BI_RGB.0,
                                    ..Default::default()
                                },
                                bmiColors: [RGBQUAD::default()],
                            };

                            // CS_OWNDC lets us cache the DC
                            let hdc = GetDC(hwnd);
                            if !hdc.is_invalid() {
                                if let Some(ref mut buf) = offscreen {
                                    buf.ensure(hdc, cw, ch);
                                } else {
                                    offscreen = OffscreenBuffer::new(hdc, cw, ch);
                                }
                                if let Some(ref buf) = offscreen {
                                    let _ = PatBlt(buf.hdc, 0, 0, cw, ch, BLACKNESS);
                                    let _ = SetStretchBltMode(buf.hdc, COLORONCOLOR);
                                    let _ = StretchDIBits(
                                        buf.hdc, dx, dy, dw, dh, 0, 0, w as i32, h as i32,
                                        Some(pkt.bgra.as_ptr() as *const _),
                                        &bmi, DIB_RGB_COLORS, SRCCOPY,
                                    );
                                    // Flip back-buffer to screen atomically → no tearing/flicker
                                    let _ = BitBlt(hdc, 0, 0, cw, ch, buf.hdc, 0, 0, SRCCOPY);
                                }
                                let _ = ReleaseDC(hwnd, hdc);
                            }
                        }
                    }
                }
            }
        } else if is_shown {
            let _ = ShowWindow(hwnd, SW_HIDE);
            is_shown = false;
            offscreen = None;
        }

        // ~60 fps polling interval
        std::thread::sleep(Duration::from_millis(8));
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

unsafe fn get_monitor_bounds(hwnd: HWND) -> (i32, i32, i32, i32) {
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if GetMonitorInfoW(monitor, &mut mi).as_bool() {
        let r = mi.rcMonitor;
        (r.left, r.top, r.right - r.left, r.bottom - r.top)
    } else {
        (0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
    }
}

unsafe fn show_window_fullscreen_or_restore(hwnd: HWND, shared: &ViewerSharedState, first_show: bool) {
    if shared.is_fullscreen.load(Ordering::Relaxed) || first_show {
        let (mx, my, mw, mh) = get_monitor_bounds(hwnd);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, mx, my, mw, mh, SWP_SHOWWINDOW | SWP_FRAMECHANGED);
        shared.is_fullscreen.store(true, Ordering::Relaxed);
    } else {
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}

unsafe fn toggle_fullscreen(hwnd: HWND, shared: &ViewerSharedState) {
    if shared.is_fullscreen.load(Ordering::Relaxed) {
        // Switch to centred windowed mode
        let (mx, my, mw, mh) = get_monitor_bounds(hwnd);
        let ww = (mw * 3 / 4).max(640);
        let wh = (mh * 3 / 4).max(480);
        let _ = SetWindowPos(
            hwnd, HWND_NOTOPMOST,
            mx + (mw - ww) / 2, my + (mh - wh) / 2,
            ww, wh,
            SWP_SHOWWINDOW | SWP_FRAMECHANGED,
        );
        shared.is_fullscreen.store(false, Ordering::Relaxed);
    } else {
        // Back to fullscreen
        let (mx, my, mw, mh) = get_monitor_bounds(hwnd);
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, mx, my, mw, mh, SWP_SHOWWINDOW | SWP_FRAMECHANGED);
        shared.is_fullscreen.store(true, Ordering::Relaxed);
    }
}

/// Return (dst_x, dst_y, dst_w, dst_h) letterboxed inside (screen_w × screen_h).
pub fn fit_frame(frame_w: u32, frame_h: u32, screen_w: i32, screen_h: i32) -> Option<(i32, i32, i32, i32)> {
    if frame_w == 0 || frame_h == 0 || screen_w <= 0 || screen_h <= 0 { return None; }
    let (sw, sh) = (screen_w as u64, screen_h as u64);
    let (fw, fh) = (frame_w as u64, frame_h as u64);
    let (dw, dh) = if sw * fh <= sh * fw {
        (sw, (sw * fh / fw).max(1))
    } else {
        ((sh * fw / fh).max(1), sh)
    };
    Some((((sw - dw) / 2) as i32, ((sh - dh) / 2) as i32, dw as i32, dh as i32))
}

/// Convert WM_MOUSEWHEEL screen-coordinates to client coords safely.
unsafe fn wheel_screen_to_client(hwnd: HWND, lparam: LPARAM) -> (i32, i32) {
    let mut pt = POINT {
        x: (lparam.0 & 0xffff) as i16 as i32,
        y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
    };
    let _ = ScreenToClient(hwnd, &mut pt);
    (pt.x, pt.y)
}

#[cfg(test)]
mod tests {
    use super::fit_frame;
    #[test]
    fn fits_different_aspect_ratios_without_stretching() {
        assert_eq!(fit_frame(1920, 1080, 1366, 768), Some((0, 0, 1365, 768)));
        assert_eq!(fit_frame(1920, 1080, 1024, 768), Some((0, 96, 1024, 576)));
        assert_eq!(fit_frame(1280, 1024, 1920, 1080), Some((285, 0, 1350, 1080)));
        assert_eq!(fit_frame(0, 1080, 1920, 1080), None);
    }
}

// ── Window procedure ──────────────────────────────────────────────────────────
unsafe extern "system" fn projection_wnd_proc(
    hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    let shared_opt = VIEWER_STATE.get();

    match msg {
        // Never let Windows erase background — avoids white flash between frames
        WM_ERASEBKGND => return LRESULT(1),

        WM_SETCURSOR => {
            SetCursor(LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default());
            return LRESULT(1);
        }

        // ── Keyboard ─────────────────────────────────────────────────────────
        WM_KEYDOWN => {
            match wparam.0 {
                k if k == VK_ESCAPE.0 as usize => {
                    if let Some(s) = shared_opt { toggle_fullscreen(hwnd, s); }
                    else { let _ = ShowWindow(hwnd, SW_HIDE); }
                    return LRESULT(0);
                }
                k if k == VK_F11_CODE => {
                    if let Some(s) = shared_opt { toggle_fullscreen(hwnd, s); }
                    return LRESULT(0);
                }
                _ => {
                    if let Some(s) = shared_opt {
                        let vk        = (wparam.0 & 0xffff) as u16;
                        let scan      = ((lparam.0 >> 16) & 0xff) as u16;
                        let extended  = (lparam.0 & (1 << 24)) != 0;
                        s.send_input_event(&InputEvent::Keyboard {
                            vk_code: vk, scan_code: scan,
                            state: KeyState::KeyDown, is_extended: extended,
                        });
                        return LRESULT(0);
                    }
                }
            }
        }
        WM_KEYUP => {
            if let Some(s) = shared_opt {
                let vk       = (wparam.0 & 0xffff) as u16;
                let scan     = ((lparam.0 >> 16) & 0xff) as u16;
                let extended = (lparam.0 & (1 << 24)) != 0;
                s.send_input_event(&InputEvent::Keyboard {
                    vk_code: vk, scan_code: scan,
                    state: KeyState::KeyUp, is_extended: extended,
                });
                return LRESULT(0);
            }
        }

        // Double-click → toggle fullscreen
        WM_LBUTTONDBLCLK => {
            if let Some(s) = shared_opt { toggle_fullscreen(hwnd, s); }
            return LRESULT(0);
        }

        // ── Mouse buttons (client coordinates in LPARAM) ──────────────────────
        WM_LBUTTONDOWN => {
            let cx = (lparam.0 & 0xffff) as i16 as i32;
            let cy = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            if let Some(s) = shared_opt {
                if let Some((px, py)) = s.map_client_to_pc(cx, cy) {
                    s.send_input_event(&InputEvent::CursorPosition { x: px, y: py });
                    s.send_input_event(&InputEvent::MouseButton { button: MouseButton::Left, state: ButtonState::Pressed });
                }
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            if let Some(s) = shared_opt {
                s.send_input_event(&InputEvent::MouseButton { button: MouseButton::Left, state: ButtonState::Released });
            }
            return LRESULT(0);
        }
        WM_RBUTTONDOWN => {
            let cx = (lparam.0 & 0xffff) as i16 as i32;
            let cy = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            if let Some(s) = shared_opt {
                if let Some((px, py)) = s.map_client_to_pc(cx, cy) {
                    s.send_input_event(&InputEvent::CursorPosition { x: px, y: py });
                    s.send_input_event(&InputEvent::MouseButton { button: MouseButton::Right, state: ButtonState::Pressed });
                }
            }
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            if let Some(s) = shared_opt {
                s.send_input_event(&InputEvent::MouseButton { button: MouseButton::Right, state: ButtonState::Released });
            }
            return LRESULT(0);
        }
        WM_MBUTTONDOWN => {
            let cx = (lparam.0 & 0xffff) as i16 as i32;
            let cy = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            if let Some(s) = shared_opt {
                if let Some((px, py)) = s.map_client_to_pc(cx, cy) {
                    s.send_input_event(&InputEvent::CursorPosition { x: px, y: py });
                    s.send_input_event(&InputEvent::MouseButton { button: MouseButton::Middle, state: ButtonState::Pressed });
                }
            }
            return LRESULT(0);
        }
        WM_MBUTTONUP => {
            if let Some(s) = shared_opt {
                s.send_input_event(&InputEvent::MouseButton { button: MouseButton::Middle, state: ButtonState::Released });
            }
            return LRESULT(0);
        }

        // ── Mouse move (only forward drags; hover moves don't need to be sent) ─
        WM_MOUSEMOVE => {
            // Forward hover too — cursor position needed for scroll hit-test on PC side
            let cx = (lparam.0 & 0xffff) as i16 as i32;
            let cy = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            if let Some(s) = shared_opt {
                if let Some((px, py)) = s.map_client_to_pc(cx, cy) {
                    s.send_input_event(&InputEvent::CursorPosition { x: px, y: py });
                }
            }
            return LRESULT(0);
        }

        // ── Scroll wheel — LPARAM has *screen* coords, convert to client first ─
        WM_MOUSEWHEEL => {
            let (cx, cy) = wheel_screen_to_client(hwnd, lparam);
            let delta    = ((wparam.0 >> 16) as i16) as i32 / 120 * 120;
            if let Some(s) = shared_opt {
                // If cursor is not over the image, find the current cursor position
                let (pcx, pcy) = if let Some(xy) = s.map_client_to_pc(cx, cy) {
                    xy
                } else {
                    // Scroll without specific hit — use last known cursor on PC
                    let meta = *s.current_meta.lock().unwrap();
                    if let Some(m) = meta {
                        let cx2 = m.offset_x as i32 + (m.width  / 2) as i32;
                        let cy2 = m.offset_y as i32 + (m.height / 2) as i32;
                        (cx2, cy2)
                    } else {
                        return LRESULT(0);
                    }
                };
                s.send_input_event(&InputEvent::CursorPosition { x: pcx, y: pcy });
                s.send_input_event(&InputEvent::MouseWheel { delta: delta as i16, horizontal: false });
            }
            return LRESULT(0);
        }
        WM_MOUSEHWHEEL => {
            let (cx, cy) = wheel_screen_to_client(hwnd, lparam);
            let delta    = ((wparam.0 >> 16) as i16) as i32 / 120 * 120;
            if let Some(s) = shared_opt {
                let (pcx, pcy) = if let Some(xy) = s.map_client_to_pc(cx, cy) {
                    xy
                } else {
                    let meta = *s.current_meta.lock().unwrap();
                    if let Some(m) = meta {
                        (m.offset_x as i32 + (m.width / 2) as i32,
                         m.offset_y as i32 + (m.height / 2) as i32)
                    } else { return LRESULT(0); }
                };
                s.send_input_event(&InputEvent::CursorPosition { x: pcx, y: pcy });
                s.send_input_event(&InputEvent::MouseWheel { delta: delta as i16, horizontal: true });
            }
            return LRESULT(0);
        }

        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
