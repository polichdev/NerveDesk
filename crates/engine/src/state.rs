use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use capture::VirtualDisplayManager;
use input::InputHookManager;
use net::{AudioBridge, FolderSync, MobileServer};

use crate::badge::DragBadge;
use crate::viewer::ProjectionViewer;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMetrics {
    pub mode: String,
    pub is_active: bool,
    pub kvm_active: bool,
    pub kvm_enabled: bool,
    pub mirror_enabled: bool,
    pub peer_online: bool,
    pub file_transfer_enabled: bool,
    pub mobile_active: bool,
    pub audio_enabled: bool,
    pub audio_sending: bool,
    pub audio_receiving: bool,
    pub folder_sync_enabled: bool,
    pub video_fps: f64,
    pub bandwidth_mbps: f64,
    pub peer_device: Option<String>,
    pub virtual_display_active: bool,
    pub projection_active: bool,
    pub screen_edge: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    pub name: String,
    pub role: String,
    pub ip: String,
    pub input_port: u16,
    pub video_port: u16,
    pub screen_width: u32,
    pub screen_height: u32,
    pub screen_edge: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureSettings {
    pub keyboard_sharing: bool,
    pub clipboard_sharing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardPacket {
    pub token: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileRemoteInfo {
    pub is_active: bool,
    pub ip: String,
    pub port: u16,
    pub url: String,
    pub qr_svg: String,
    pub pin: String,
}

pub struct AppState {
    pub is_kvm_active: Arc<AtomicBool>,
    pub is_projecting: Arc<AtomicBool>,
    pub kvm_enabled: Arc<AtomicBool>,
    pub mirror_enabled: Arc<AtomicBool>,
    pub mode: Mutex<String>,
    pub peer_device: Arc<Mutex<Option<String>>>,
    pub screen_edge: Arc<Mutex<String>>,
    pub display_manager: VirtualDisplayManager,
    pub hook_manager: Mutex<Option<InputHookManager>>,
    pub kvm_workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    pub projection_workers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    pub live_fps_x10: Arc<AtomicU64>,
    pub received_video_fps_x10: Arc<AtomicU64>,
    pub received_mirror_fps_x10: Arc<AtomicU64>,
    pub live_mbps_x10: Arc<AtomicU64>,
    pub discovered_devices: Arc<Mutex<HashMap<String, DiscoveredDevice>>>,
    pub device_last_seen: Arc<Mutex<HashMap<String, Instant>>>,
    pub keyboard_sharing: Arc<AtomicBool>,
    pub clipboard_sharing: Arc<AtomicBool>,
    pub paired_security_key: Arc<Mutex<Option<String>>>,
    pub paired_peer_ip: Arc<Mutex<Option<String>>>,
    pub clipboard_hash: Arc<AtomicU64>,
    pub projection_viewer: Arc<ProjectionViewer>,
    pub projection_cancel: Arc<AtomicBool>,
    pub projection_generation: Arc<AtomicU64>,
    pub file_transfer_enabled: Arc<AtomicBool>,
    pub drag_badge: Arc<DragBadge>,
    pub mobile_server: Arc<MobileServer>,
    pub audio_bridge: Arc<AudioBridge>,
    pub folder_sync: Arc<FolderSync>,
}

impl AppState {
    pub fn new() -> Self {
        let projection_viewer = Arc::new(ProjectionViewer::new());
        let drag_badge = Arc::new(DragBadge::new());
        let mobile_server = Arc::new(MobileServer::new(5858));
        let device_last_seen = Arc::new(Mutex::new(HashMap::new()));
        let paired_security_key = Arc::new(Mutex::new(None));
        let paired_peer_ip = Arc::new(Mutex::new(None));
        let folder_sync = FolderSync::new();
        folder_sync.start_saved();

        Self {
            is_kvm_active: Arc::new(AtomicBool::new(false)),
            is_projecting: Arc::new(AtomicBool::new(false)),
            kvm_enabled: Arc::new(AtomicBool::new(false)),
            mirror_enabled: Arc::new(AtomicBool::new(false)),
            mode: Mutex::new("Idle".into()),
            peer_device: Arc::new(Mutex::new(None)),
            screen_edge: Arc::new(Mutex::new("Right".into())),
            display_manager: VirtualDisplayManager::new(),
            hook_manager: Mutex::new(None),
            kvm_workers: Mutex::new(Vec::new()),
            projection_workers: Mutex::new(Vec::new()),
            live_fps_x10: Arc::new(AtomicU64::new(0)),
            received_video_fps_x10: Arc::new(AtomicU64::new(0)),
            received_mirror_fps_x10: Arc::new(AtomicU64::new(0)),
            live_mbps_x10: Arc::new(AtomicU64::new(0)),
            discovered_devices: Arc::new(Mutex::new(HashMap::new())),
            device_last_seen: device_last_seen.clone(),
            keyboard_sharing: Arc::new(AtomicBool::new(true)),
            clipboard_sharing: Arc::new(AtomicBool::new(true)),
            paired_security_key,
            paired_peer_ip,
            clipboard_hash: Arc::new(AtomicU64::new(0)),
            projection_viewer,
            projection_cancel: Arc::new(AtomicBool::new(false)),
            projection_generation: Arc::new(AtomicU64::new(0)),
            file_transfer_enabled: Arc::new(AtomicBool::new(true)),
            drag_badge,
            mobile_server,
            audio_bridge: Arc::new(AudioBridge::new(device_last_seen)),
            folder_sync,
        }
    }
}

pub fn update_mode_string(state: &AppState) {
    let kvm = state.is_kvm_active.load(Ordering::SeqCst);
    let proj = state.is_projecting.load(Ordering::SeqCst);
    let m = match (kvm, proj) {
        (true, true) => "KVM & Projection Active",
        (true, false) => "KVM Active",
        (false, true) => "Projection Active",
        (false, false) => "Idle",
    };
    *state.mode.lock().unwrap() = m.into();
}

pub fn stop_kvm_internal(state: &AppState) {
    state.is_kvm_active.store(false, Ordering::SeqCst);
    if let Ok(mut workers) = state.kvm_workers.lock() {
        for w in workers.drain(..) {
            w.abort();
        }
    }
    input::set_peer_available(false);
    if let Ok(mut guard) = state.hook_manager.lock() {
        if let Some(mut hook) = guard.take() {
            hook.stop();
        }
    }
    update_mode_string(state);
}

pub fn stop_projection_internal(state: &AppState) {
    state.projection_cancel.store(true, Ordering::SeqCst);
    state.projection_generation.fetch_add(1, Ordering::SeqCst);
    state.is_projecting.store(false, Ordering::SeqCst);
    state.projection_viewer.stop();
    let _ = state.display_manager.unplug_display();
    if let Ok(mut workers) = state.projection_workers.lock() {
        for w in workers.drain(..) {
            w.abort();
        }
    }
    state.live_fps_x10.store(0, Ordering::Relaxed);
    state.live_mbps_x10.store(0, Ordering::Relaxed);
    update_mode_string(state);
}

pub async fn resolve_target_ip(target_ip: &str, state: &AppState) -> Result<String, String> {
    let host_part = if let Some((h, _)) = target_ip.split_once(':') {
        h
    } else {
        target_ip
    };

    if let Ok(ip) = host_part.parse::<std::net::IpAddr>() {
        return Ok(ip.to_string());
    }

    if let Ok(guard) = state.discovered_devices.lock() {
        for d in guard.values() {
            if d.name.eq_ignore_ascii_case(host_part) {
                return Ok(d.ip.clone());
            }
        }
    }

    if let Ok(mut addrs) = tokio::net::lookup_host(format!("{host_part}:48100")).await {
        if let Some(addr) = addrs.next() {
            return Ok(addr.ip().to_string());
        }
    }

    Err(format!("Device not reachable: {host_part}"))
}

pub fn get_screen_dimensions() -> (u32, u32) {
    unsafe {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Gdi::{
            GetDC, GetDeviceCaps, ReleaseDC, DESKTOPHORZRES, DESKTOPVERTRES,
        };
        let hdc = GetDC(HWND::default());
        let mut w = 0;
        let mut h = 0;
        if !hdc.is_invalid() {
            w = GetDeviceCaps(hdc, DESKTOPHORZRES) as u32;
            h = GetDeviceCaps(hdc, DESKTOPVERTRES) as u32;
            let _ = ReleaseDC(HWND::default(), hdc);
        }
        if w == 0 || h == 0 {
            w = windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN,
            ) as u32;
            h = windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
                windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN,
            ) as u32;
        }
        (if w > 0 { w } else { 1920 }, if h > 0 { h } else { 1080 })
    }
}
