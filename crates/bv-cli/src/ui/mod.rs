//! On-screen UI for choosing what to record and showing that a recording
//! is running. Windows only for now.

// The picker and its options bar only exist on Windows so far; elsewhere
// their types are defined but unused.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::PathBuf;

use anyhow::Result;
use bv_core::{CaptureRegion, OutputFormat};

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

/// Output size/frame-rate presets offered in the options bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Small,
    Medium,
    Large,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Small, Quality::Medium, Quality::Large];

    /// Maximum output width (smaller selections keep their own width).
    pub fn width(self) -> u32 {
        match self {
            Quality::Small => 640,
            Quality::Medium => 960,
            Quality::Large => 1280,
        }
    }

    pub fn fps(self) -> u32 {
        match self {
            Quality::Small => 10,
            Quality::Medium => 12,
            Quality::Large => 15,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Quality::Small => "Small",
            Quality::Medium => "Medium",
            Quality::Large => "Large",
        }
    }

    /// The preset closest to an existing `--width`.
    pub fn from_width(width: u32) -> Self {
        match width {
            0..=720 => Quality::Small,
            721..=1080 => Quality::Medium,
            _ => Quality::Large,
        }
    }
}

/// What the options bar lets the user change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choices {
    pub format: OutputFormat,
    pub quality: Quality,
    pub clicks: bool,
    pub zoom: bool,
}

pub struct PickRequest {
    /// Initial state of the options bar.
    pub choices: Choices,
    /// Shown in the bar so it's clear where the file will go.
    pub save_dir: PathBuf,
    /// Seconds of 3-2-1 before recording starts (0 = none).
    pub countdown: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct Picked {
    pub region: CaptureRegion,
    pub choices: Choices,
}

/// Dim every screen and let the user click one or drag out an area, adjust
/// options, and press Record. `None` if they cancel.
#[cfg(windows)]
pub fn pick(req: PickRequest) -> Result<Option<Picked>> {
    // Its own thread: the picker runs a message loop that must not pump
    // the caller's windows (e.g. the tray's event loop).
    let choice = std::thread::Builder::new()
        .name("bv-picker".into())
        .spawn(move || win::picker::run(req))?
        .join()
        .map_err(|_| anyhow::anyhow!("area picker crashed"))?;
    // Let the overlay finish disappearing before anything grabs the screen.
    std::thread::sleep(std::time::Duration::from_millis(150));
    Ok(choice)
}

#[cfg(not(windows))]
pub fn pick(_req: PickRequest) -> Result<Option<Picked>> {
    anyhow::bail!("--select is only available on Windows so far; use --region or --monitor")
}

/// A frame around the area being recorded with a Stop button, hidden from
/// the recording itself. Closes when dropped.
pub struct Indicator {
    #[cfg(windows)]
    inner: win::indicator::Indicator,
}

impl Indicator {
    /// Show the frame around `region`. `None` where unsupported.
    pub fn show(region: CaptureRegion) -> Option<Self> {
        #[cfg(windows)]
        {
            win::indicator::Indicator::show(region).map(|inner| Self { inner })
        }
        #[cfg(not(windows))]
        {
            let _ = region;
            None
        }
    }

    /// Whether the Stop button was clicked.
    pub fn stop_requested(&self) -> bool {
        #[cfg(windows)]
        {
            self.inner.stop_requested()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}
