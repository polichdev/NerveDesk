use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tracing::{info, warn};

use common::{
    ButtonState, InputEvent, KeyState, MouseButton, NerveError, Result,
};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use crate::edge::{ActiveLocation, EdgeConfig, EdgeDetector, ScreenEdge, TransitionResult};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetDesktopWindow, GetMessageW, GetSystemMetrics, PeekMessageW, PostThreadMessageW,
    SetCursorPos, SetForegroundWindow, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
    MSLLHOOKSTRUCT, MSG, PM_NOREMOVE, SM_CXSCREEN, SM_CYSCREEN, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

const XBUTTON1: u32 = 0x0001;
const XBUTTON2: u32 = 0x0002;

static HOOK_SENDER: Mutex<Option<Sender<InputEvent>>> = Mutex::new(None);
static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static KEYBOARD_SHARING_ENABLED: AtomicBool = AtomicBool::new(true);
static LAST_CURSOR_POS: Mutex<Option<POINT>> = Mutex::new(None);
static EDGE_DETECTOR: Mutex<Option<EdgeDetector>> = Mutex::new(None);
static CENTER_PT: Mutex<POINT> = Mutex::new(POINT { x: 960, y: 540 });

pub fn set_keyboard_sharing(enabled: bool) {
    KEYBOARD_SHARING_ENABLED.store(enabled, Ordering::SeqCst);
}

pub fn is_keyboard_sharing_enabled() -> bool {
    KEYBOARD_SHARING_ENABLED.load(Ordering::SeqCst)
}

pub fn set_active_edge(edge: ScreenEdge) {
    set_active_edge_and_resolution(edge, 0, 0);
}

pub fn set_active_edge_and_resolution(edge: ScreenEdge, client_w: i32, client_h: i32) {
    let (w, h) = unsafe {
        use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC, GetDeviceCaps, DESKTOPHORZRES, DESKTOPVERTRES};
        let hdc = GetDC(HWND::default());
        let mut dw = 0;
        let mut dh = 0;
        if !hdc.is_invalid() {
            dw = GetDeviceCaps(hdc, DESKTOPHORZRES);
            dh = GetDeviceCaps(hdc, DESKTOPVERTRES);
            let _ = ReleaseDC(HWND::default(), hdc);
        }
        if dw > 0 && dh > 0 {
            (dw, dh)
        } else {
            (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
        }
    };
    let host_w = if w > 0 { w } else { 1920 };
    let host_h = if h > 0 { h } else { 1080 };
    let cw = if client_w > 0 { client_w } else { host_w };
    let ch = if client_h > 0 { client_h } else { host_h };

    if let Ok(mut center) = CENTER_PT.lock() {
        *center = POINT {
            x: host_w / 2,
            y: host_h / 2,
        };
    }
    let config = EdgeConfig {
        host_width: host_w,
        host_height: host_h,
        client_width: cw,
        client_height: ch,
        client_edge: edge,
        trigger_threshold: 4,
    };
    if let Ok(mut guard) = EDGE_DETECTOR.lock() {
        *guard = Some(EdgeDetector::new(config));
    }
}

const WM_STOP_HOOK: u32 = WM_APP + 42;

unsafe extern "system" fn mouse_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if n_code >= 0 {
        let msg = w_param.0 as u32;
        let hook_struct = *(l_param.0 as *const MSLLHOOKSTRUCT);

        let mut event_to_send = None;
        let mut block_event = false;

        let center = if let Ok(guard) = CENTER_PT.lock() {
            *guard
        } else {
            POINT { x: 960, y: 540 }
        };

        if msg == WM_MOUSEMOVE {
            let is_injected = (hook_struct.flags & 1) != 0;
            let is_at_center = hook_struct.pt.x == center.x && hook_struct.pt.y == center.y;

            if CAPTURE_ACTIVE.load(Ordering::SeqCst) {
                if is_injected || is_at_center {
                    return LRESULT(1);
                }

                let dx = hook_struct.pt.x - center.x;
                let dy = hook_struct.pt.y - center.y;

                let _ = SetCursorPos(center.x, center.y);

                let mut transition = None;
                if let Ok(mut edge_guard) = EDGE_DETECTOR.lock() {
                    if let Some(detector) = edge_guard.as_mut() {
                        transition = detector.evaluate_motion(center.x, center.y, dx, dy);
                    }
                }

                if let Some(TransitionResult::ReturnedToHost { return_x, return_y }) = transition {
                    CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
                    let _ = SetCursorPos(return_x, return_y);
                    if let Ok(mut guard) = LAST_CURSOR_POS.lock() {
                        *guard = Some(POINT { x: return_x, y: return_y });
                    }
                } else if dx != 0 || dy != 0 {
                    let cur_pos = if let Ok(guard) = EDGE_DETECTOR.lock() {
                        guard.as_ref().map(|d| d.virtual_client_pos())
                    } else {
                        None
                    };
                    if let Some((vx, vy)) = cur_pos {
                        event_to_send = Some(InputEvent::CursorPosition { x: vx, y: vy });
                    } else {
                        event_to_send = Some(InputEvent::MouseMove { dx, dy });
                    }
                }

                block_event = true;
            } else {
                let current_pt = hook_struct.pt;
                let mut dx = 0;
                let mut dy = 0;
                if let Ok(mut guard) = LAST_CURSOR_POS.lock() {
                    if let Some(last_pt) = *guard {
                        dx = current_pt.x - last_pt.x;
                        dy = current_pt.y - last_pt.y;
                    }
                    *guard = Some(current_pt);
                }

                let mut transition = None;
                if let Ok(mut edge_guard) = EDGE_DETECTOR.lock() {
                    if let Some(detector) = edge_guard.as_mut() {
                        transition = detector.evaluate_motion(current_pt.x, current_pt.y, dx, dy);
                    }
                }

                if let Some(TransitionResult::CrossedToClient { client_x, client_y }) = transition {
                    CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
                    let _ = SetCursorPos(center.x, center.y);
                    let _ = SetForegroundWindow(GetDesktopWindow());
                    event_to_send = Some(InputEvent::CursorPosition { x: client_x, y: client_y });
                    block_event = true;
                }
            }
        } else if CAPTURE_ACTIVE.load(Ordering::Relaxed) {
            match msg {
                WM_LBUTTONDOWN => {
                    event_to_send = Some(InputEvent::MouseButton {
                        button: MouseButton::Left,
                        state: ButtonState::Pressed,
                    });
                    block_event = true;
                }
                WM_LBUTTONUP => {
                    event_to_send = Some(InputEvent::MouseButton {
                        button: MouseButton::Left,
                        state: ButtonState::Released,
                    });
                    block_event = true;
                }
                WM_RBUTTONDOWN => {
                    event_to_send = Some(InputEvent::MouseButton {
                        button: MouseButton::Right,
                        state: ButtonState::Pressed,
                    });
                    block_event = true;
                }
                WM_RBUTTONUP => {
                    event_to_send = Some(InputEvent::MouseButton {
                        button: MouseButton::Right,
                        state: ButtonState::Released,
                    });
                    block_event = true;
                }
                WM_MBUTTONDOWN => {
                    event_to_send = Some(InputEvent::MouseButton {
                        button: MouseButton::Middle,
                        state: ButtonState::Pressed,
                    });
                    block_event = true;
                }
                WM_MBUTTONUP => {
                    event_to_send = Some(InputEvent::MouseButton {
                        button: MouseButton::Middle,
                        state: ButtonState::Released,
                    });
                    block_event = true;
                }
                WM_XBUTTONDOWN => {
                    let hiword = ((hook_struct.mouseData >> 16) & 0xFFFF) as u16;
                    let btn = if hiword == XBUTTON1 as u16 {
                        MouseButton::X1
                    } else if hiword == XBUTTON2 as u16 {
                        MouseButton::X2
                    } else {
                        MouseButton::X2
                    };
                    event_to_send = Some(InputEvent::MouseButton {
                        button: btn,
                        state: ButtonState::Pressed,
                    });
                    block_event = true;
                }
                WM_XBUTTONUP => {
                    let hiword = ((hook_struct.mouseData >> 16) & 0xFFFF) as u16;
                    let btn = if hiword == XBUTTON1 as u16 {
                        MouseButton::X1
                    } else if hiword == XBUTTON2 as u16 {
                        MouseButton::X2
                    } else {
                        MouseButton::X2
                    };
                    event_to_send = Some(InputEvent::MouseButton {
                        button: btn,
                        state: ButtonState::Released,
                    });
                    block_event = true;
                }
                WM_MOUSEWHEEL => {
                    let delta = ((hook_struct.mouseData >> 16) as i16) as i16;
                    event_to_send = Some(InputEvent::MouseWheel {
                        delta,
                        horizontal: false,
                    });
                    block_event = true;
                }
                WM_MOUSEHWHEEL => {
                    let delta = ((hook_struct.mouseData >> 16) as i16) as i16;
                    event_to_send = Some(InputEvent::MouseWheel {
                        delta,
                        horizontal: true,
                    });
                    block_event = true;
                }
                _ => {}
            }
        }

        if let Some(event) = event_to_send {
            if let Ok(guard) = HOOK_SENDER.lock() {
                if let Some(sender) = guard.as_ref() {
                    let _ = sender.try_send(event);
                }
            }
        }

        if block_event {
            return LRESULT(1);
        }
    }

    CallNextHookEx(HHOOK::default(), n_code, w_param, l_param)
}

unsafe extern "system" fn keyboard_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if n_code >= 0 && CAPTURE_ACTIVE.load(Ordering::Relaxed) && KEYBOARD_SHARING_ENABLED.load(Ordering::Relaxed) {
        let msg = w_param.0 as u32;
        let hook_struct = *(l_param.0 as *const KBDLLHOOKSTRUCT);

        if hook_struct.vkCode == 0x1B {
            let shift_down = (windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x10) as u16 & 0x8000) != 0;
            if shift_down {
                CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
                if let Ok(mut guard) = EDGE_DETECTOR.lock() {
                    if let Some(detector) = guard.as_mut() {
                        detector.set_location(ActiveLocation::Host);
                    }
                }
                if let Ok(mut guard) = LAST_CURSOR_POS.lock() {
                    *guard = None;
                }
                let center = if let Ok(guard) = CENTER_PT.lock() {
                    *guard
                } else {
                    POINT { x: 960, y: 540 }
                };
                let _ = SetCursorPos(center.x, center.y);
                return CallNextHookEx(HHOOK::default(), n_code, w_param, l_param);
            }
        }

        let state = match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => Some(KeyState::KeyDown),
            WM_KEYUP | WM_SYSKEYUP => Some(KeyState::KeyUp),
            _ => None,
        };

        if let Some(key_state) = state {
            let is_extended = (hook_struct.flags.0 & 1) != 0;
            let event = InputEvent::Keyboard {
                vk_code: hook_struct.vkCode as u16,
                scan_code: hook_struct.scanCode as u16,
                state: key_state,
                is_extended,
            };

            if let Ok(guard) = HOOK_SENDER.lock() {
                if let Some(sender) = guard.as_ref() {
                    let _ = sender.try_send(event);
                }
            }

            return LRESULT(1);
        }
    }

    CallNextHookEx(HHOOK::default(), n_code, w_param, l_param)
}

pub struct InputHookManager {
    thread_handle: Option<JoinHandle<()>>,
    thread_id: u32,
    is_capturing: Arc<AtomicBool>,
}

impl InputHookManager {
    pub fn start(event_sender: Sender<InputEvent>) -> Result<Self> {
        set_active_edge(ScreenEdge::Right);
        let (init_tx, init_rx) = crossbeam_channel::bounded::<Result<u32>>(1);
        let is_capturing = Arc::new(AtomicBool::new(false));
        let is_capturing_clone = is_capturing.clone();

        let handle = std::thread::Builder::new()
            .name("nerve-input-hook".into())
            .spawn(move || unsafe {
                let current_thread = GetCurrentThread();
                if let Err(e) = SetThreadPriority(current_thread, THREAD_PRIORITY_TIME_CRITICAL) {
                    warn!("Failed to elevate hook thread priority: {:?}", e);
                }

                let tid = windows::Win32::System::Threading::GetCurrentThreadId();
                if let Ok(mut guard) = HOOK_SENDER.lock() {
                    *guard = Some(event_sender);
                }
                CAPTURE_ACTIVE.store(false, Ordering::SeqCst);

                let mouse_hook = match SetWindowsHookExW(
                    WH_MOUSE_LL,
                    Some(mouse_proc),
                    HINSTANCE::default(),
                    0,
                ) {
                    Ok(hook) => hook,
                    Err(e) => {
                        let _ = init_tx.send(Err(NerveError::Platform(format!(
                            "Failed to install mouse hook: {e}"
                        ))));
                        return;
                    }
                };

                let kbd_hook = match SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(keyboard_proc),
                    HINSTANCE::default(),
                    0,
                ) {
                    Ok(hook) => hook,
                    Err(e) => {
                        let _ = UnhookWindowsHookEx(mouse_hook);
                        let _ = init_tx.send(Err(NerveError::Platform(format!(
                            "Failed to install keyboard hook: {e}"
                        ))));
                        return;
                    }
                };

                let _ = init_tx.send(Ok(tid));

                let mut msg = MSG::default();
                let _ = PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_NOREMOVE);

                info!("NerveDesk input hooks successfully installed");

                while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                    if msg.message == WM_STOP_HOOK {
                        break;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                let _ = UnhookWindowsHookEx(mouse_hook);
                let _ = UnhookWindowsHookEx(kbd_hook);
                if let Ok(mut guard) = HOOK_SENDER.lock() {
                    *guard = None;
                }
                CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
                if let Ok(mut guard) = LAST_CURSOR_POS.lock() {
                    *guard = None;
                }
                is_capturing_clone.store(false, Ordering::SeqCst);
                info!("NerveDesk input hooks safely removed");
            })
            .map_err(|e| NerveError::Platform(format!("Failed to spawn hook thread: {e}")))?;

        let thread_id = init_rx
            .recv()
            .map_err(|_| NerveError::Platform("Hook thread terminated prematurely".into()))??;

        Ok(Self {
            thread_handle: Some(handle),
            thread_id,
            is_capturing,
        })
    }

    pub fn set_capturing(&self, active: bool) {
        CAPTURE_ACTIVE.store(active, Ordering::SeqCst);
        self.is_capturing.store(active, Ordering::SeqCst);
        if let Ok(mut guard) = EDGE_DETECTOR.lock() {
            if let Some(detector) = guard.as_mut() {
                detector.set_location(if active { ActiveLocation::Client } else { ActiveLocation::Host });
            }
        }
        if active {
            let center = if let Ok(guard) = CENTER_PT.lock() {
                *guard
            } else {
                POINT { x: 960, y: 540 }
            };
            unsafe {
                let _ = SetCursorPos(center.x, center.y);
                let _ = SetForegroundWindow(GetDesktopWindow());
            }
        } else {
            if let Ok(mut guard) = LAST_CURSOR_POS.lock() {
                *guard = None;
            }
        }
    }

    pub fn is_capturing(&self) -> bool {
        self.is_capturing.load(Ordering::SeqCst)
    }

    pub fn stop(&mut self) {
        if let Some(handle) = self.thread_handle.take() {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_STOP_HOOK, WPARAM(0), LPARAM(0));
            }
            let _ = handle.join();
        }
    }
}

impl Drop for InputHookManager {
    fn drop(&mut self) {
        self.stop();
    }
}