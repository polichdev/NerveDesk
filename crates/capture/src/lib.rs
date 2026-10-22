pub mod driver;
pub mod dxgi;
pub mod virtual_display;
pub mod window;

pub use driver::{
    elevate as elevate_driver, handle_cli as handle_driver_cli,
    is_installed as is_driver_installed, package_available as is_driver_package_available,
};
pub use dxgi::{DxgiCaptureConfig, DxgiOutputDuplicator, GpuFrame};
pub use virtual_display::VirtualDisplayManager;
pub use window::{list_windows, CaptureApartment, CaptureWindow, WindowCapture};
