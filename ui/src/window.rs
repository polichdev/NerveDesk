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
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIcon, GetWindowLongPtrW, LoadImageW, SendMessageW, SetClassLongPtrW, SetWindowLongPtrW,
        SetWindowPos, GCLP_HICON, GCLP_HICONSM, GWL_EXSTYLE, ICON_BIG, ICON_SMALL, IMAGE_ICON,
        LR_DEFAULTSIZE, LR_SHARED, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SWP_NOZORDER, WM_SETICON, WS_EX_APPWINDOW, WS_EX_DLGMODALFRAME, WS_EX_TOOLWINDOW,
    };

    let hwnd = HWND(raw_hwnd);
    let dark: u32 = 1;
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            (style | WS_EX_DLGMODALFRAME.0 as isize | WS_EX_APPWINDOW.0 as isize)
                & !(WS_EX_TOOLWINDOW.0 as isize),
        );

        let and_mask = [0xFFu8; 32];
        let xor_mask = [0x00u8; 32];
        let transparent_icon =
            CreateIcon(None, 16, 16, 1, 1, and_mask.as_ptr(), xor_mask.as_ptr()).ok();

        let icon_param = transparent_icon
            .map(|h| h.0 as isize)
            .unwrap_or(0);

        let _ = SendMessageW(
            hwnd,
            WM_SETICON,
            WPARAM(ICON_SMALL as usize),
            LPARAM(icon_param),
        );
        SetClassLongPtrW(hwnd, GCLP_HICONSM, icon_param);

        if let Ok(hinstance) = GetModuleHandleW(None) {
            if let Ok(big_icon) = LoadImageW(
                hinstance,
                windows::core::PCWSTR(1 as usize as *const u16),
                IMAGE_ICON,
                0,
                0,
                LR_DEFAULTSIZE | LR_SHARED,
            ) {
                let big_param = big_icon.0 as isize;
                let _ = SendMessageW(
                    hwnd,
                    WM_SETICON,
                    WPARAM(ICON_BIG as usize),
                    LPARAM(big_param),
                );
                SetClassLongPtrW(hwnd, GCLP_HICON, big_param);
            }
        }

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
