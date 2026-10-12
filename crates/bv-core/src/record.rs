//! Screen recording into a lossless-ish intermediate file.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::ffmpeg::{self, Platform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl std::str::FromStr for CaptureRegion {
    type Err = String;

    /// Parses `WIDTHxHEIGHT+X+Y` (X11 geometry style), e.g. `1280x720+0+0`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || format!("invalid region {s:?}, expected WIDTHxHEIGHT+X+Y");
        let (size, rest) = s.split_once('+').ok_or_else(err)?;
        let (w, h) = size.split_once('x').ok_or_else(err)?;
        let (x, y) = rest.split_once('+').ok_or_else(err)?;
        let n = |v: &str| v.trim().parse::<u32>().map_err(|_| err());
        Ok(Self {
            x: n(x)?,
            y: n(y)?,
            width: n(w)?,
            height: n(h)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct RecordOptions {
    pub output: PathBuf,
    pub fps: u32,
    pub region: Option<CaptureRegion>,
    /// Platform-specific screen id: X11 display (`:0`), avfoundation device
    /// name, or gdigrab target. `None` picks the main screen.
    pub display: Option<String>,
    /// Stop automatically after this long.
    pub max_duration: Option<Duration>,
    /// Burn the OS cursor into the video.
    pub draw_cursor: bool,
}

impl RecordOptions {
    pub fn new(output: impl Into<PathBuf>) -> Self {
        Self {
            output: output.into(),
            fps: 30,
            region: None,
            display: None,
            max_duration: None,
            draw_cursor: true,
        }
    }
}

/// A running recording. Call [`Recording::stop`] to finalize the file.
pub struct Recording {
    child: Child,
    started: Instant,
    started_wall: SystemTime,
    max_duration: Option<Duration>,
    output: PathBuf,
}

/// A finished recording.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub path: PathBuf,
    /// Seconds between [`Recording::started`] and the first captured frame
    /// (ffmpeg startup latency). Subtract it from cursor timestamps taken
    /// relative to `started` to line them up with the video. 0 if unknown.
    pub first_frame_delay: f64,
}

impl Recording {
    pub fn start(opts: &RecordOptions) -> Result<Self> {
        let platform = Platform::current();
        let mut cmd = std::process::Command::new(ffmpeg::ffmpeg_bin());
        // stdin stays open: writing "q" is ffmpeg's graceful stop.
        // Stamp frames with wall-clock time and keep those timestamps in the
        // file, so we can tell exactly when the first frame was grabbed.
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-use_wallclock_as_timestamps", "1"])
            .args(ffmpeg::capture_input_args(platform, opts))
            .arg("-copyts");
        cmd.args(["-vf", &ffmpeg::capture_filter(platform, opts)])
            // Fast, visually lossless intermediate; the export step makes it small.
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-crf",
                "16",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&opts.output)
            .stdin(Stdio::piped());

        let (started, started_wall) = (Instant::now(), SystemTime::now());
        let child = cmd
            .spawn()
            .context("starting ffmpeg (is it installed and on PATH?)")?;
        Ok(Self {
            child,
            started,
            started_wall,
            // Enforced here rather than with ffmpeg's `-t`, which counts from
            // timestamp zero and so never stops correctly with `-copyts`.
            max_duration: opts.max_duration,
            output: opts.output.clone(),
        })
    }

    /// When ffmpeg was launched; use as the origin for cursor timestamps.
    pub fn started(&self) -> Instant {
        self.started
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn output(&self) -> &Path {
        &self.output
    }

    /// True once `max_duration` has passed or ffmpeg exited by itself.
    pub fn finished(&mut self) -> bool {
        self.max_duration.is_some_and(|d| self.elapsed() >= d)
            || matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Ask ffmpeg to finish writing and wait for it.
    pub fn stop(mut self) -> Result<Recorded> {
        if let Some(stdin) = self.child.stdin.as_mut() {
            // Ignore errors: ffmpeg may already have exited.
            let _ = stdin.write_all(b"q");
            let _ = stdin.flush();
        }
        let status = self.child.wait().context("waiting for ffmpeg")?;
        // ffmpeg exits 255 when interrupted by SIGINT (Ctrl+C) but still
        // finalizes the file, so trust the output if it exists.
        if !status.success() && !self.output.exists() {
            bail!("ffmpeg recording failed ({status})");
        }
        let launched = self
            .started_wall
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let first_frame_delay = ffmpeg::probe(&self.output)
            .map(|info| first_frame_delay(launched, info.start_time))
            .unwrap_or(0.0);
        Ok(Recorded {
            path: self.output,
            first_frame_delay,
        })
    }
}

/// Delay between launching ffmpeg and its first frame, given both as Unix
/// times. Falls back to 0 when the file's timestamps aren't wall-clock
/// (some capture devices ignore `-use_wallclock_as_timestamps`).
fn first_frame_delay(launched: f64, first_frame: f64) -> f64 {
    let delay = first_frame - launched;
    if (0.0..10.0).contains(&delay) {
        delay
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_region() {
        let r: CaptureRegion = "1280x720+10+20".parse().unwrap();
        assert_eq!(
            r,
            CaptureRegion {
                x: 10,
                y: 20,
                width: 1280,
                height: 720
            }
        );
        assert!("1280x720".parse::<CaptureRegion>().is_err());
    }

    #[test]
    fn first_frame_delay_ignores_non_wallclock_timestamps() {
        assert!((first_frame_delay(1_700_000_000.0, 1_700_000_000.25) - 0.25).abs() < 1e-6);
        assert_eq!(first_frame_delay(1_700_000_000.0, 0.0), 0.0);
        assert_eq!(first_frame_delay(1_700_000_000.0, 1_699_999_999.0), 0.0);
    }
}
