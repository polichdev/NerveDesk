pub mod edge;
pub mod emulator;
pub mod hook;

pub use edge::{ActiveLocation, EdgeConfig, EdgeDetector, ScreenEdge, TransitionResult};
pub use emulator::InputEmulator;
pub use hook::{
    disable_edge_detection, is_keyboard_sharing_enabled, is_lbutton_down, opposite_edge,
    restore_edge_detection, set_active_edge, set_active_edge_and_resolution,
    set_active_edges_and_resolution, set_drag_resolver, set_keyboard_sharing, set_peer_available,
    set_remote_edge, take_cached_drag_files, take_drag_source_window, InputHookManager,
};
