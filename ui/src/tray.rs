use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const PANEL_WIDTH: f64 = 286.0;
const PANEL_HEIGHT: f64 = 310.0;

pub fn setup_tray(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let panel = WebviewWindowBuilder::new(app, "tray-panel", WebviewUrl::App("tray.html".into()))
        .title("NerveDesk")
        .inner_size(PANEL_WIDTH, PANEL_HEIGHT)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .build()?;
    let _ = panel.set_theme(Some(tauri::Theme::Dark));

    let mut builder = TrayIconBuilder::new()
        .tooltip("NerveDesk")
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left | MouseButton::Right,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(panel) = tray.app_handle().get_webview_window("tray-panel") {
                    toggle_panel(&panel);
                }
            }
        });

    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    let _tray = builder.build(app)?;
    Ok(())
}

fn toggle_panel(panel: &WebviewWindow) {
    if panel.is_visible().unwrap_or(false) {
        let _ = panel.hide();
        return;
    }

    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

        let mut cursor = POINT::default();
        if GetCursorPos(&mut cursor).is_ok() {
            let scale = panel.scale_factor().unwrap_or(1.0);
            let width = (PANEL_WIDTH * scale) as i32;
            let height = (PANEL_HEIGHT * scale) as i32;
            let (mut x, mut y) = (cursor.x - width + 24, cursor.y - height - 12);
            if let Ok(monitors) = panel.available_monitors() {
                if let Some(monitor) = monitors.into_iter().find(|monitor| {
                    let pos = monitor.position();
                    let size = monitor.size();
                    cursor.x >= pos.x
                        && cursor.x < pos.x + size.width as i32
                        && cursor.y >= pos.y
                        && cursor.y < pos.y + size.height as i32
                }) {
                    let pos = monitor.position();
                    let size = monitor.size();
                    x = x.clamp(pos.x + 8, pos.x + size.width as i32 - width - 8);
                    y = y.clamp(pos.y + 8, pos.y + size.height as i32 - height - 8);
                }
            }
            let _ = panel.set_position(PhysicalPosition::new(x, y));
        }
    }

    let _ = panel.show();
    let _ = panel.set_focus();
}
