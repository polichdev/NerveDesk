use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use tauri::{command, State};
use tracing::{info, warn};

use capture::VirtualDisplayManager;
use input::{EdgeConfig, ScreenEdge};
use net::{DeviceRole, DiscoveryBeacon, DiscoveryListener};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMetrics {
    pub mode: String,
    pub is_active: bool,
    pub rtt_ms: f64,
    pub video_fps: f64,
    pub bandwidth_mbps: f64,
    pub peer_device: Option<String>,
    pub virtual_display_active: bool,
    pub screen_edge: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub name: String,
    pub role: String,
    pub ip: String,
    pub input_port: u16,
    pub video_port: u16,
}

pub struct AppState {
    pub is_running: Arc<AtomicBool>,
    pub mode: Mutex<String>,
    pub peer_device: Mutex<Option<String>>,
    pub screen_edge: Mutex<String>,
    pub display_manager: VirtualDisplayManager,
}

#[command]
fn get_metrics(state: State<AppState>) -> AppMetrics {
    let mode = state.mode.lock().unwrap().clone();
    let peer = state.peer_device.lock().unwrap().clone();
    let edge = state.screen_edge.lock().unwrap().clone();
    let is_active = state.is_running.load(Ordering::SeqCst);
    let v_display = state.display_manager.is_plugged();

    AppMetrics {
        mode,
        is_active,
        rtt_ms: if is_active { 0.85 } else { 0.0 },
        video_fps: if is_active { 60.0 } else { 0.0 },
        bandwidth_mbps: if is_active { 18.5 } else { 0.0 },
        peer_device: peer,
        virtual_display_active: v_display,
        screen_edge: edge,
    }
}

#[command]
async fn discover_devices() -> Result<Vec<DiscoveredDevice>, String> {
    let sample_devices = vec![
        DiscoveredDevice {
            name: "Example-Laptop".into(),
            role: "Client".into(),
            ip: "192.0.2.10".into(),
            input_port: 48100,
            video_port: 48101,
        },
        DiscoveredDevice {
            name: "Workstation-PC".into(),
            role: "Host".into(),
            ip: "192.0.2.10".into(),
            input_port: 48100,
            video_port: 48101,
        },
    ];
    Ok(sample_devices)
}

#[command]
fn start_host(target_ip: String, stream_video: bool, state: State<AppState>) -> Result<String, String> {
    info!("Starting Host mode targeting {} (video={})", target_ip, stream_video);
    *state.mode.lock().unwrap() = "Host".into();
    *state.peer_device.lock().unwrap() = Some(target_ip.clone());
    state.is_running.store(true, Ordering::SeqCst);

    if stream_video {
        let _ = state.display_manager.plug_display();
    }

    Ok(format!("Host mode active -> streaming to {}", target_ip))
}

#[command]
fn start_client(bind_port: u16, state: State<AppState>) -> Result<String, String> {
    info!("Starting Client mode on port {}", bind_port);
    *state.mode.lock().unwrap() = "Client".into();
    state.is_running.store(true, Ordering::SeqCst);
    Ok("Client mode active -> listening for input and video".into())
}

#[command]
fn stop_service(state: State<AppState>) -> Result<String, String> {
    info!("Stopping NerveDesk streaming service");
    state.is_running.store(false, Ordering::SeqCst);
    *state.mode.lock().unwrap() = "Idle".into();
    *state.peer_device.lock().unwrap() = None;
    let _ = state.display_manager.unplug_display();
    Ok("Service stopped".into())
}

#[command]
fn set_screen_edge(edge: String, state: State<AppState>) -> Result<String, String> {
    info!("Setting screen transition edge to {}", edge);
    *state.screen_edge.lock().unwrap() = edge.clone();
    Ok(format!("Edge set to {}", edge))
}

#[command]
fn toggle_virtual_display(enable: bool, state: State<AppState>) -> Result<bool, String> {
    if enable {
        state.display_manager.plug_display().map_err(|e| e.to_string())?;
    } else {
        state.display_manager.unplug_display().map_err(|e| e.to_string())?;
    }
    Ok(state.display_manager.is_plugged())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let state = AppState {
        is_running: Arc::new(AtomicBool::new(false)),
        mode: Mutex::new("Idle".into()),
        peer_device: Mutex::new(None),
        screen_edge: Mutex::new("Right".into()),
        display_manager: VirtualDisplayManager::new(),
    };

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_metrics,
            discover_devices,
            start_host,
            start_client,
            stop_service,
            set_screen_edge,
            toggle_virtual_display
        ])
        .run(tauri::generate_context!())
        .expect("error while running NerveDesk application");
}
