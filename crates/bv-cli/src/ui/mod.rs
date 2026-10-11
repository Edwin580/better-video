//! On-screen UI for choosing what to record. Windows only for now.

use anyhow::Result;
use bv_core::CaptureRegion;

#[cfg(windows)]
mod win;

/// The user backed out of a picker.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Dim every screen and let the user click one or drag out an area.
/// `None` if they cancel.
#[cfg(windows)]
pub fn pick_region() -> Result<Option<CaptureRegion>> {
    // Its own thread: the picker runs a message loop that must not pump
    // the caller's windows (e.g. the tray's event loop).
    let choice = std::thread::Builder::new()
        .name("bv-picker".into())
        .spawn(win::picker::run)?
        .join()
        .map_err(|_| anyhow::anyhow!("area picker crashed"))?;
    // Let the overlay finish disappearing before anything grabs the screen.
    std::thread::sleep(std::time::Duration::from_millis(150));
    Ok(choice)
}

#[cfg(not(windows))]
pub fn pick_region() -> Result<Option<CaptureRegion>> {
    anyhow::bail!("--select is only available on Windows so far; use --region or --monitor")
}
