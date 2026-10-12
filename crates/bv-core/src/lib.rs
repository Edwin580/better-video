//! Core of better-video: everything that isn't platform UI.
//!
//! The pipeline is intentionally two-stage so recording stays cheap:
//!
//! 1. **Record** — the native engine (`bv-engine`, Windows) or ffmpeg grabs
//!    the screen into a fast intermediate (`.bvraw` / `.mkv`) while the
//!    caller samples the cursor into a [`cursor::CursorTrack`].
//! 2. **Export** — the intermediate is decoded, optionally run through the
//!    click-planned [`zoom::ZoomPlanner`] + [`render`] crop/scale, and
//!    re-encoded into a small GIF / MP4 / WebM suitable for a PR body.

pub mod cursor;
pub mod export;
pub mod ffmpeg;
pub mod geometry;
pub mod monitor;
mod native;
pub mod overlay;
pub mod pointer;
pub mod record;
pub mod render;
pub mod style;
pub mod window;
pub mod zoom;

pub use cursor::{CursorSample, CursorTrack};
pub use export::{export, ExportOptions, Exported, OutputFormat};
pub use geometry::{Rect, Size};
pub use monitor::{Monitor, MonitorChoice};
pub use pointer::PointerStyle;
pub use record::{CaptureRegion, Engine, RecordOptions, Recorded, Recording};
pub use style::{Background, FrameStyle, StylePreset};
pub use zoom::{ZoomConfig, ZoomPlanner};
