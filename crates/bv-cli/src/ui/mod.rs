//! On-screen UI for choosing what to record and showing that a recording
//! is running. Windows only for now.

// The picker and its options bar only exist on Windows so far; elsewhere
// their types are defined but unused.
#![cfg_attr(not(windows), allow(dead_code))]

use std::path::PathBuf;

use anyhow::Result;
use bv_core::{zoom::ZoomTrigger, CaptureRegion, OutputFormat};
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// Zoom choices offered in the options bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ZoomMode {
    Off,
    Click,
    Hover,
    Follow,
}

impl ZoomMode {
    pub const ALL: [ZoomMode; 4] = [
        ZoomMode::Off,
        ZoomMode::Click,
        ZoomMode::Hover,
        ZoomMode::Follow,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ZoomMode::Off => "Off",
            ZoomMode::Click => "Click",
            ZoomMode::Hover => "Hover",
            ZoomMode::Follow => "Follow",
        }
    }

    pub fn trigger(self) -> Option<ZoomTrigger> {
        match self {
            ZoomMode::Off => None,
            ZoomMode::Click => Some(ZoomTrigger::Click),
            ZoomMode::Hover => Some(ZoomTrigger::Hover),
            ZoomMode::Follow => Some(ZoomTrigger::Follow),
        }
    }

    pub fn from_trigger(t: Option<ZoomTrigger>) -> Self {
        match t {
            None => ZoomMode::Off,
            Some(ZoomTrigger::Click) => ZoomMode::Click,
            Some(ZoomTrigger::Hover | ZoomTrigger::ClickOrHover) => ZoomMode::Hover,
            Some(ZoomTrigger::Follow) => ZoomMode::Follow,
        }
    }
}

/// Also reads settings saved when zoom was an on/off switch.
impl<'de> Deserialize<'de> for ZoomMode {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Switch(bool),
            Name(String),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Switch(on) => {
                if on {
                    ZoomMode::Click
                } else {
                    ZoomMode::Off
                }
            }
            Raw::Name(n) => ZoomMode::ALL
                .into_iter()
                .find(|m| m.label().eq_ignore_ascii_case(&n))
                .ok_or_else(|| serde::de::Error::custom(format!("unknown zoom mode {n:?}")))?,
        })
    }
}

/// What the options bar lets the user change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choices {
    pub format: OutputFormat,
    pub quality: Quality,
    pub clicks: bool,
    pub zoom: ZoomMode,
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

/// What the recording pill asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopRequest {
    /// Stop and save the clip.
    Save,
    /// Stop and throw the clip away.
    Discard,
}

/// A frame around the area being recorded, with a pill holding Stop and
/// Discard; hidden from the recording itself. Closes when dropped.
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

    /// Whether Stop or Discard was clicked on the pill.
    pub fn requested(&self) -> Option<StopRequest> {
        #[cfg(windows)]
        {
            self.inner.requested()
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}

/// Sets a notification's icon.
#[cfg(feature = "tray")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Saved,
    Error,
}

/// A notification in the corner of the main screen; clicking it shows
/// `reveal` in the file manager. Doesn't block.
#[cfg(feature = "tray")]
pub fn toast(kind: ToastKind, title: &str, body: &str, reveal: Option<PathBuf>) {
    #[cfg(windows)]
    win::toast::show(kind, title.to_owned(), body.to_owned(), reveal);
    #[cfg(not(windows))]
    {
        let _ = (kind, reveal);
        eprintln!("{title} — {body}");
    }
}

/// Show `path` selected in the file manager (or open it, if a folder).
#[cfg(feature = "tray")]
pub fn reveal(path: &std::path::Path) {
    #[cfg(windows)]
    win::toast::reveal(path);
    #[cfg(not(windows))]
    {
        let dir = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        if let Err(e) = std::process::Command::new(program).arg(dir).spawn() {
            eprintln!("error: couldn't run {program}: {e}");
        }
    }
}

/// Ask for a folder with the system dialog (Windows). Blocks; `None` if
/// cancelled or unsupported.
#[cfg(all(feature = "tray", windows))]
pub fn choose_folder(start: &std::path::Path) -> Option<PathBuf> {
    win::folder::choose(start)
}

#[cfg(all(feature = "tray", not(windows)))]
pub fn choose_folder(_start: &std::path::Path) -> Option<PathBuf> {
    None
}
