//! Core of better-video: everything that isn't platform UI.
//!
//! The pipeline is intentionally two-stage so recording stays cheap:
//!
//! 1. **Record** — ffmpeg grabs the screen into a fast, high-quality
//!    intermediate (`.mkv`) while the caller samples the cursor into a
//!    [`cursor::CursorTrack`].
//! 2. **Export** — the intermediate is decoded, optionally run through the
//!    cursor-following [`zoom::ZoomPlanner`] + [`render`] crop/scale, and
//!    re-encoded into a small GIF / MP4 / WebM suitable for a PR body.

pub mod cursor;
pub mod export;
pub mod ffmpeg;
pub mod geometry;
pub mod record;
pub mod render;
pub mod zoom;

pub use cursor::{CursorSample, CursorTrack};
pub use export::{export, ExportOptions, OutputFormat};
pub use geometry::{Rect, Size};
pub use record::{CaptureRegion, RecordOptions, Recorded, Recording};
pub use zoom::{ZoomConfig, ZoomPlanner};
