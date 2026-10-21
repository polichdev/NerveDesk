pub mod edge;
pub mod emulator;
pub mod hook;

pub use edge::{ActiveLocation, EdgeConfig, EdgeDetector, ScreenEdge, TransitionResult};
pub use emulator::InputEmulator;
pub use hook::{
    disable_edge_detection, restore_edge_detection,
    is_keyboard_sharing_enabled, is_lbutton_down,
    set_active_edge, set_active_edge_and_resolution,
    set_keyboard_sharing, InputHookManager,
};