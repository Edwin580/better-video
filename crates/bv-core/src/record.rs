//! Screen recording into a lossless-ish intermediate file.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    time::{Duration, Instant},
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
    output: PathBuf,
}

impl Recording {
    pub fn start(opts: &RecordOptions) -> Result<Self> {
        let platform = Platform::current();
        let mut cmd = std::process::Command::new(ffmpeg::ffmpeg_bin());
        // stdin stays open: writing "q" is ffmpeg's graceful stop.
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(ffmpeg::capture_input_args(platform, opts));
        if let Some(d) = opts.max_duration {
            cmd.args(["-t", &format!("{:.3}", d.as_secs_f64())]);
        }
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

        let child = cmd
            .spawn()
            .context("starting ffmpeg (is it installed and on PATH?)")?;
        Ok(Self {
            child,
            started: Instant::now(),
            output: opts.output.clone(),
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn output(&self) -> &Path {
        &self.output
    }

    /// True once ffmpeg exited by itself (duration reached or an error).
    pub fn finished(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Ask ffmpeg to finish writing and wait for it.
    pub fn stop(mut self) -> Result<PathBuf> {
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
        Ok(self.output)
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
}
