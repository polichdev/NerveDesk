use tauri::{App, Manager, WebviewWindow};

pub fn configure_main_window(app: &mut App) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_theme(Some(tauri::Theme::Dark));
        let _ = window.set_title(" ");
        #[cfg(target_os = "windows")]
        apply_windows_dark_theme(&window);

        if std::env::args().any(|arg| arg == "--minimized") {
            let _ = window.hide();
        }
    }
}

#[cfg(target_os = "windows")]
fn apply_windows_dark_theme(window: &WebviewWindow) {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SendMessageW, SetClassLongPtrW, SetWindowLongPtrW, SetWindowPos,
        GCLP_HICON, GCLP_HICONSM, GWL_EXSTYLE, ICON_BIG, ICON_SMALL, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_SETICON, WS_EX_DLGMODALFRAME,
    };

    if let Ok(raw_hwnd) = window.hwnd() {
        let hwnd = HWND(raw_hwnd.0 as *mut std::ffi::c_void);
        let dark: u32 = 1;
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_DLGMODALFRAME.0 as isize);
            let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_SMALL as usize), LPARAM(0));
            let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_BIG as usize), LPARAM(0));
            SetClassLongPtrW(hwnd, GCLP_HICONSM, 0);
            SetClassLongPtrW(hwnd, GCLP_HICON, 0);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark as *const _ as _,
                std::mem::size_of::<u32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(19),
                &dark as *const _ as _,
                std::mem::size_of::<u32>() as u32,
            );
            let caption_color: u32 = 0x00141212;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(35),
                &caption_color as *const _ as _,
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}
