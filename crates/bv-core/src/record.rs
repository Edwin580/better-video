//! Screen recording into a lossless-ish intermediate file.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use bv_engine::capture::Area;

use crate::ffmpeg::{self, Platform};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRegion {
    /// Offsets in virtual-desktop pixels; negative for monitors left of or
    /// above the primary one.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl std::str::FromStr for CaptureRegion {
    type Err = String;

    /// Parses `WIDTHxHEIGHT+X+Y` (X11 geometry style), e.g. `1280x720+0+0`.
    /// Either offset may use `-` instead of `+`: `1920x1080-1920+0`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || format!("invalid region {s:?}, expected WIDTHxHEIGHT+X+Y");
        let s = s.trim();
        let split = s.find(['+', '-']).ok_or_else(err)?;
        let (size, offsets) = s.split_at(split);
        let (w, h) = size.split_once('x').ok_or_else(err)?;
        // The second sign starts the Y offset.
        let second = offsets[1..].find(['+', '-']).ok_or_else(err)? + 1;
        let (x, y) = offsets.split_at(second);
        let dim = |v: &str| match v.trim().parse::<u32>() {
            Ok(n) if n > 0 => Ok(n),
            _ => Err(err()),
        };
        // `i32::from_str` accepts a leading `+` or `-`.
        let off = |v: &str| v.parse::<i32>().map_err(|_| err());
        Ok(Self {
            x: off(x)?,
            y: off(y)?,
            width: dim(w)?,
            height: dim(h)?,
        })
    }
}

impl std::fmt::Display for CaptureRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}x{}{:+}{:+}", self.width, self.height, self.x, self.y)
    }
}

/// What grabs the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// bv-engine: GDI capture into a `.bvraw` (Windows only). GIF export
    /// needs no ffmpeg at all.
    Native,
    /// ffmpeg's platform grabber into an `.mkv`.
    Ffmpeg,
}

impl Engine {
    /// Native where it's implemented, ffmpeg elsewhere.
    pub fn default_for_platform() -> Self {
        if cfg!(windows) {
            Engine::Native
        } else {
            Engine::Ffmpeg
        }
    }

    /// Extension of the intermediate recording.
    pub fn raw_extension(self) -> &'static str {
        match self {
            Engine::Native => bv_engine::raw::EXTENSION,
            Engine::Ffmpeg => "mkv",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordOptions {
    pub output: PathBuf,
    pub fps: u32,
    pub region: Option<CaptureRegion>,
    /// Platform-specific screen id: X11 display (`:0`), avfoundation device
    /// name, or gdigrab target. `None` picks the main screen. ffmpeg only.
    pub display: Option<String>,
    /// Stop automatically after this long.
    pub max_duration: Option<Duration>,
    /// Burn the OS cursor into the video.
    pub draw_cursor: bool,
    pub engine: Engine,
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
            engine: Engine::default_for_platform(),
        }
    }
}

/// A running recording. Call [`Recording::stop`] to finalize the file.
pub struct Recording {
    backend: Backend,
    started: Instant,
    started_wall: SystemTime,
    max_duration: Option<Duration>,
    output: PathBuf,
}

enum Backend {
    Ffmpeg(Child),
    Native(bv_engine::Recorder),
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
        let (started, started_wall) = (Instant::now(), SystemTime::now());
        let backend = match opts.engine {
            Engine::Ffmpeg => Backend::Ffmpeg(start_ffmpeg(opts)?),
            Engine::Native => {
                let area = match opts.region {
                    Some(r) => Area {
                        x: r.x,
                        y: r.y,
                        width: r.width,
                        height: r.height,
                    },
                    None => bv_engine::capture::virtual_screen(),
                };
                let rec = bv_engine::Recorder::start(
                    opts.output.clone(),
                    area,
                    opts.fps,
                    opts.draw_cursor,
                    started,
                )
                .context("starting screen capture")?;
                Backend::Native(rec)
            }
        };
        Ok(Self {
            backend,
            started,
            started_wall,
            // Enforced here rather than with ffmpeg's `-t`, which counts from
            // timestamp zero and so never stops correctly with `-copyts`.
            max_duration: opts.max_duration,
            output: opts.output.clone(),
        })
    }

    /// When capture was launched; use as the origin for cursor timestamps.
    pub fn started(&self) -> Instant {
        self.started
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn output(&self) -> &Path {
        &self.output
    }

    /// True once `max_duration` has passed or capture stopped by itself.
    pub fn finished(&mut self) -> bool {
        self.max_duration.is_some_and(|d| self.elapsed() >= d)
            || match &mut self.backend {
                Backend::Ffmpeg(child) => matches!(child.try_wait(), Ok(Some(_))),
                Backend::Native(rec) => rec.is_finished(),
            }
    }

    /// Finish writing the recording and wait for it.
    pub fn stop(self) -> Result<Recorded> {
        let first_frame_delay = match self.backend {
            Backend::Native(rec) => rec.stop().context("screen capture failed")?.first_frame,
            Backend::Ffmpeg(child) => stop_ffmpeg(child, &self.output, self.started_wall)?,
        };
        Ok(Recorded {
            path: self.output,
            first_frame_delay,
        })
    }
}

fn start_ffmpeg(opts: &RecordOptions) -> Result<Child> {
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
    cmd.spawn()
        .context("starting ffmpeg (is it installed and on PATH?)")
}

/// Ask ffmpeg to finish writing; returns the first-frame delay.
fn stop_ffmpeg(mut child: Child, output: &Path, started_wall: SystemTime) -> Result<f64> {
    if let Some(stdin) = child.stdin.as_mut() {
        // Ignore errors: ffmpeg may already have exited.
        let _ = stdin.write_all(b"q");
        let _ = stdin.flush();
    }
    let status = child.wait().context("waiting for ffmpeg")?;
    // ffmpeg exits 255 when interrupted by SIGINT (Ctrl+C) but still
    // finalizes the file, so trust the output if it exists.
    if !status.success() && !output.exists() {
        bail!("ffmpeg recording failed ({status})");
    }
    let launched = started_wall
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    Ok(ffmpeg::probe(output)
        .map(|info| first_frame_delay(launched, info.start_time))
        .unwrap_or(0.0))
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
        assert!("0x720+0+0".parse::<CaptureRegion>().is_err());
    }

    #[test]
    fn parses_negative_offsets_and_round_trips() {
        let r: CaptureRegion = "1920x1080-1920+0".parse().unwrap();
        assert_eq!((r.x, r.y), (-1920, 0));
        let r: CaptureRegion = "800x600+5-1080".parse().unwrap();
        assert_eq!((r.x, r.y), (5, -1080));
        assert_eq!(r.to_string(), "800x600+5-1080");
        assert_eq!(r.to_string().parse::<CaptureRegion>(), Ok(r));
    }

    #[test]
    fn first_frame_delay_ignores_non_wallclock_timestamps() {
        assert!((first_frame_delay(1_700_000_000.0, 1_700_000_000.25) - 0.25).abs() < 1e-6);
        assert_eq!(first_frame_delay(1_700_000_000.0, 0.0), 0.0);
        assert_eq!(first_frame_delay(1_700_000_000.0, 1_699_999_999.0), 0.0);
    }
}
