pub mod badge;
pub mod mirror_worker;
pub mod services;
pub mod state;
pub mod viewer;

pub use badge::DragBadge;
pub use services::{engine_async_spawn, start_background_services, FileReceivedCallback};
pub use state::{
    get_screen_dimensions, resolve_target_ip, stop_kvm_internal, stop_projection_internal,
    update_mode_string, AppMetrics, AppState, ClipboardPacket, DiscoveredDevice, FeatureSettings,
    MobileRemoteInfo,
};
pub use viewer::ProjectionViewer;
