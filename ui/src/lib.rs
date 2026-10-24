use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};

pub use capture::handle_driver_cli;
pub use engine::AppState;
use net::{FileTransferServer, FILE_TRANSFER_PORT};

mod bluetooth_audio;
mod commands;
mod firewall;
mod secret_store;
mod tray;
mod window;

pub use firewall::handle_cli as handle_firewall_cli;

pub fn run() {
    let state = AppState::new();
    if let Ok(Some(key)) = secret_store::load_key() {
        if let Ok(mut saved) = state.paired_security_key.lock() {
            *saved = Some(key);
        }
    }
    let app_handle_for_drop = Arc::new(Mutex::new(None::<tauri::AppHandle>));
    let app_handle_for_setup = app_handle_for_drop.clone();
    let app_handle_for_cb = app_handle_for_drop.clone();

    engine::start_background_services(
        &state,
        Some(Arc::new(move |paths: Vec<String>| {
            if let Ok(guard) = app_handle_for_cb.lock() {
                if let Some(ref handle) = *guard {
                    let _ = handle.emit("file-received", paths);
                }
            }
        })),
    );

    if let Err(e) = tauri::Builder::default()
        .manage(state)
        .setup(move |app| {
            if let Ok(mut guard) = app_handle_for_setup.lock() {
                *guard = Some(app.handle().clone());
            }

            let file_key_ref = app.state::<AppState>().paired_security_key.clone();
            let file_peer_ref = app.state::<AppState>().paired_peer_ip.clone();
            let file_enabled = app.state::<AppState>().file_transfer_enabled.clone();
            let file_app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let cb = move |files: Vec<std::path::PathBuf>| {
                    let paths: Vec<String> = files
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    let _ = file_app_handle.emit("file-received", paths);
                };
                let _ = FileTransferServer::start(
                    FILE_TRANSFER_PORT,
                    file_key_ref,
                    file_peer_ref,
                    file_enabled,
                    cb,
                )
                .await;
            });

            window::configure_main_window(app);
            tray::setup_tray(app)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "tray-panel" && matches!(event, tauri::WindowEvent::Focused(false))
            {
                let _ = window.hide();
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
            #[cfg(target_os = "windows")]
            if window.label() == "main" && matches!(event, tauri::WindowEvent::Focused(true)) {
                window::apply_windows_dark_theme_window(window);
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_device_name,
            commands::quit_app,
            commands::show_main_window,
            commands::get_my_ip,
            commands::get_metrics,
            commands::discover_devices,
            commands::get_paired_peer_ip,
            commands::pair_device,
            commands::prepare_pairing_key,
            commands::get_saved_pairing_key,
            commands::clear_pairing_key,
            commands::start_host,
            commands::stop_service,
            commands::set_module_enabled,
            commands::set_screen_edge,
            commands::toggle_virtual_display,
            commands::set_keyboard_sharing_enabled,
            commands::set_clipboard_sharing_enabled,
            commands::get_feature_settings,
            commands::open_display_settings,
            commands::install_virtual_display_driver,
            commands::uninstall_virtual_display_driver,
            commands::get_display_driver_status,
            commands::get_display_driver_package_status,
            commands::list_capture_windows,
            commands::start_virtual_display_cast,
            commands::stop_virtual_display_cast,
            commands::open_received_folder,
            commands::send_file_to_peer,
            commands::set_file_transfer_enabled,
            commands::get_autostart_enabled,
            commands::set_autostart_enabled,
            commands::get_mobile_remote_info,
            commands::regenerate_mobile_pin,
            commands::toggle_mobile_remote,
            commands::set_mobile_audio_enabled,
            commands::get_mobile_share_info,
            commands::share_mobile_text,
            commands::clear_mobile_share,
            commands::pick_mobile_share_file,
            commands::get_mobile_receive_folder,
            commands::set_mobile_receive_folder,
            commands::reset_mobile_receive_folder,
            commands::open_mobile_received_folder,
            commands::open_mobile_remote_url,
            commands::get_audio_status,
            commands::set_audio_enabled,
            commands::start_audio_stream,
            commands::stop_audio_stream,
            commands::request_audio_stream,
            commands::stop_requested_audio_stream,
            bluetooth_audio::list_bluetooth_audio_devices,
            bluetooth_audio::connect_bluetooth_audio,
            bluetooth_audio::reconnect_bluetooth_audio,
            bluetooth_audio::disconnect_bluetooth_audio,
            bluetooth_audio::get_bluetooth_audio_status,
            bluetooth_audio::open_bluetooth_settings,
            bluetooth_audio::open_sound_settings,
            commands::get_folder_sync_status,
            commands::generate_folder_sync_code,
            commands::set_folder_sync_enabled,
            commands::add_folder_sync_pair,
            commands::remove_folder_sync_pair,
            commands::sync_folder_now,
            commands::preview_folder_sync,
            commands::approve_folder_sync_pair,
            commands::pick_folder_sync_directory,
            commands::restore_folder_sync_version
        ])
        .run(tauri::generate_context!())
    {
        unsafe {
            use windows::core::HSTRING;
            use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
            let msg = format!(
                "Failed to launch NerveDesk GUI: {}\n\nNote: If WebView2 is missing, install Microsoft Edge WebView2 Runtime.",
                e
            );
            let _ = MessageBoxW(
                None,
                &HSTRING::from(msg),
                &HSTRING::from("NerveDesk Error"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}
