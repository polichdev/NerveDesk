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
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT,
    MSLLHOOKSTRUCT, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP,
    WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

const XBUTTON1: u32 = 0x0001;
const XBUTTON2: u32 = 0x0002;

static HOOK_SENDER: Mutex<Option<Sender<InputEvent>>> = Mutex::new(None);
static CAPTURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static LAST_CURSOR_POS: Mutex<Option<POINT>> = Mutex::new(None);

const WM_STOP_HOOK: u32 = WM_APP + 42;

unsafe extern "system" fn mouse_proc(n_code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if n_code >= 0 && CAPTURE_ACTIVE.load(Ordering::Relaxed) {
        let msg = w_param.0 as u32;
        let hook_struct = *(l_param.0 as *const MSLLHOOKSTRUCT);

        let mut event_to_send = None;
        let mut block_event = false;

        match msg {
            WM_MOUSEMOVE => {
                let current_pt = hook_struct.pt;
                if let Ok(mut guard) = LAST_CURSOR_POS.lock() {
                    if let Some(last_pt) = *guard {
                        let dx = current_pt.x - last_pt.x;
                        let dy = current_pt.y - last_pt.y;
                        if dx != 0 || dy != 0 {
                            event_to_send = Some(InputEvent::MouseMove { dx, dy });
                        }
                    }
                    *guard = Some(current_pt);
                }
                block_event = true;
            }
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
    if n_code >= 0 && CAPTURE_ACTIVE.load(Ordering::Relaxed) {
        let msg = w_param.0 as u32;
        let hook_struct = *(l_param.0 as *const KBDLLHOOKSTRUCT);

        if hook_struct.vkCode == 0x1B || hook_struct.vkCode == 0x13 || hook_struct.vkCode == 0x91 {
            let shift_down = (windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x10) as u16 & 0x8000) != 0;
            let ctrl_down = (windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(0x11) as u16 & 0x8000) != 0;
            if shift_down || ctrl_down || hook_struct.vkCode == 0x13 || hook_struct.vkCode == 0x91 {
                CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
                if let Ok(mut guard) = LAST_CURSOR_POS.lock() {
                    *guard = None;
                }
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
        if !active {
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
