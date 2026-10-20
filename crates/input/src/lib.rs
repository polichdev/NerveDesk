pub mod edge;
pub mod emulator;
pub mod hook;

pub use edge::{ActiveLocation, EdgeConfig, EdgeDetector, ScreenEdge, TransitionResult};
pub use emulator::InputEmulator;
pub use hook::{set_active_edge, set_active_edge_and_resolution, InputHookManager};