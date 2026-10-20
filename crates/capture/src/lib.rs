pub mod dxgi;
pub mod virtual_display;

pub use dxgi::{DxgiCaptureConfig, DxgiOutputDuplicator, GpuFrame};
pub use virtual_display::VirtualDisplayManager;
