use common::{NerveError, Result};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP,
};

pub const NERVEDESK_HARDWARE_ID: &str = "Root\\NerveDeskDisplay";
pub const NERVEDESK_DEVICE_DESC: &str = "NerveDesk Virtual Display";

pub struct VirtualDisplayManager;

impl VirtualDisplayManager {
    pub fn new() -> Self {
        Self
    }

    pub fn plug_display(&self) -> Result<()> {
        if self.is_plugged() {
            Ok(())
        } else {
            Err(NerveError::Platform(
                "NerveDesk display is not attached. Install the signed NerveDesk driver and extend the desktop in Windows Display Settings.".into(),
            ))
        }
    }

    pub fn unplug_display(&self) -> Result<()> {
        Ok(())
    }

    pub fn is_plugged(&self) -> bool {
        for index in 0..32 {
            let mut device = DISPLAY_DEVICEW::default();
            device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            let found = unsafe { EnumDisplayDevicesW(None, index, &mut device, 0) };
            if !found.as_bool() {
                break;
            }
            let name_end = device
                .DeviceString
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(device.DeviceString.len());
            let name = String::from_utf16_lossy(&device.DeviceString[..name_end]);
            if name.contains(NERVEDESK_DEVICE_DESC)
                && device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != 0
            {
                return true;
            }
        }
        false
    }
}

impl Default for VirtualDisplayManager {
    fn default() -> Self {
        Self::new()
    }
}
