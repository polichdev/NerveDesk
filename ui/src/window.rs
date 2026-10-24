use tauri::{App, Manager, WebviewWindow, Window};

pub fn configure_main_window(app: &mut App) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_theme(Some(tauri::Theme::Dark));
        let _ = window.set_title(" ");
        if let Some(icon) = app.default_window_icon() {
            let _ = window.set_icon(icon.clone());
        }
        #[cfg(target_os = "windows")]
        apply_windows_dark_theme_webview(&window);

        if std::env::args().any(|arg| arg == "--minimized") {
            let _ = window.hide();
        }
    }
}

#[cfg(target_os = "windows")]
pub fn apply_windows_dark_theme_webview(window: &WebviewWindow) {
    if let Ok(raw_hwnd) = window.hwnd() {
        apply_windows_dark_theme_raw(raw_hwnd.0 as _);
    }
}

#[cfg(target_os = "windows")]
pub fn apply_windows_dark_theme_window(window: &Window) {
    if let Ok(raw_hwnd) = window.hwnd() {
        apply_windows_dark_theme_raw(raw_hwnd.0 as _);
    }
}

#[cfg(target_os = "windows")]
pub fn apply_windows_dark_theme_raw(raw_hwnd: *mut std::ffi::c_void) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    };

    let hwnd = HWND(raw_hwnd);
    let dark: u32 = 1;
    unsafe {
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
