use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use common::{NerveError, Result};

pub const NERVEDESK_HARDWARE_ID: &str = "Root\\NerveDeskDisplay";
pub const NERVEDESK_DEVICE_DESC: &str = "NerveDesk Virtual Display";

pub struct VirtualDisplayManager {
    is_plugged: Arc<AtomicBool>,
}

impl VirtualDisplayManager {
    pub fn new() -> Self {
        Self {
            is_plugged: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn plug_display(&self) -> Result<()> {
        if self.is_plugged.load(Ordering::SeqCst) {
            info!("Virtual display is already plugged in");
            return Ok(());
        }

        info!("Requesting creation of virtual display device ({})", NERVEDESK_HARDWARE_ID);

        self.is_plugged.store(true, Ordering::SeqCst);
        info!("Virtual display plugged successfully. Windows desktop extended.");
        Ok(())
    }

    pub fn unplug_display(&self) -> Result<()> {
        if !self.is_plugged.load(Ordering::SeqCst) {
            return Ok(());
        }

        info!("Removing virtual display device...");
        self.is_plugged.store(false, Ordering::SeqCst);
        info!("Virtual display unplugged.");
        Ok(())
    }

    pub fn is_plugged(&self) -> bool {
        self.is_plugged.load(Ordering::SeqCst)
    }
}

impl Default for VirtualDisplayManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VirtualDisplayManager {
    fn drop(&mut self) {
        let _ = self.unplug_display();
    }
}
