use common::{ButtonState, InputEvent, KeyState, MouseButton, Result};
use tracing::warn;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    VIRTUAL_KEY, XBUTTON1, XBUTTON2,
};

pub struct InputEmulator;

impl InputEmulator {
    pub fn new() -> Self {
        Self
    }

    pub fn inject(&self, event: &InputEvent) -> Result<()> {
        match event {
            InputEvent::MouseMove { dx, dy } => self.inject_mouse_move(*dx, *dy),
            InputEvent::MouseButton { button, state } => {
                self.inject_mouse_button(*button, *state)
            }
            InputEvent::MouseWheel { delta, horizontal } => {
                self.inject_mouse_wheel(*delta, *horizontal)
            }
            InputEvent::Keyboard {
                vk_code,
                scan_code,
                state,
                is_extended,
            } => self.inject_keyboard(*vk_code, *scan_code, *state, *is_extended),
            _ => Ok(()),
        }
    }

    fn inject_mouse_move(&self, dx: i32, dy: i32) -> Result<()> {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            let sent = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
            if sent == 0 {
                warn!("SendInput (MouseMove) returned 0 injected events");
            }
        }
        Ok(())
    }

    fn inject_mouse_button(&self, button: MouseButton, state: ButtonState) -> Result<()> {
        let (flags, mouse_data) = match (button, state) {
            (MouseButton::Left, ButtonState::Pressed) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, ButtonState::Released) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, ButtonState::Pressed) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, ButtonState::Released) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, ButtonState::Pressed) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, ButtonState::Released) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::X1, ButtonState::Pressed) => (MOUSEEVENTF_XDOWN, XBUTTON1),
            (MouseButton::X1, ButtonState::Released) => (MOUSEEVENTF_XUP, XBUTTON1),
            (MouseButton::X2, ButtonState::Pressed) => (MOUSEEVENTF_XDOWN, XBUTTON2),
            (MouseButton::X2, ButtonState::Released) => (MOUSEEVENTF_XUP, XBUTTON2),
        };

        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: mouse_data as u32,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
        Ok(())
    }

    fn inject_mouse_wheel(&self, delta: i16, horizontal: bool) -> Result<()> {
        let flags = if horizontal {
            MOUSEEVENTF_HWHEEL
        } else {
            MOUSEEVENTF_WHEEL
        };

        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: (delta as i32) as u32,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
        Ok(())
    }

    fn inject_keyboard(
        &self,
        vk_code: u16,
        scan_code: u16,
        state: KeyState,
        is_extended: bool,
    ) -> Result<()> {
        let mut flags = KEYEVENTF_SCANCODE;
        if state == KeyState::KeyUp {
            flags |= KEYEVENTF_KEYUP;
        }
        if is_extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }

        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk_code),
                    wScan: scan_code,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        }
        Ok(())
    }
}

impl Default for InputEmulator {
    fn default() -> Self {
        Self::new()
    }
}
