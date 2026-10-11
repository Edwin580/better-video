//! Thin wrappers around the `ffmpeg` / `ffprobe` binaries.
//!
//! ffmpeg is the capture + codec backend for now: it already knows how to
//! grab the screen on every desktop OS and encodes GIF/MP4/WebM well. Native
//! capture backends can replace the capture half later without touching
//! the export pipeline.

use std::{
    env,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::{geometry::Size, record::RecordOptions};

/// `ffmpeg` binary to run; override with `BV_FFMPEG`.
pub fn ffmpeg_bin() -> String {
    env::var("BV_FFMPEG").unwrap_or_else(|_| "ffmpeg".into())
}

/// `ffprobe` binary to run; override with `BV_FFPROBE`.
pub fn ffprobe_bin() -> String {
    env::var("BV_FFPROBE").unwrap_or_else(|_| "ffprobe".into())
}

pub fn ffmpeg() -> Command {
    let mut cmd = Command::new(ffmpeg_bin());
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    cmd
}

/// Version line of the ffmpeg binary, if it can be run.
pub fn version() -> Option<String> {
    let out = Command::new(ffmpeg_bin()).arg("-version").output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(str::to_owned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

/// Input-side ffmpeg arguments for grabbing the screen.
pub fn capture_input_args(platform: Platform, opts: &RecordOptions) -> Vec<String> {
    let fps = opts.fps.to_string();
    let cursor = if opts.draw_cursor { "1" } else { "0" };
    let mut a: Vec<String> = Vec::new();
    let mut push = |s: &str| a.push(s.to_owned());
    match platform {
        Platform::Linux => {
            let display = opts
                .display
                .clone()
                .or_else(|| env::var("DISPLAY").ok())
                .unwrap_or_else(|| ":0".into());
            push("-f");
            push("x11grab");
            push("-framerate");
            push(&fps);
            push("-draw_mouse");
            push(cursor);
            if let Some(r) = opts.region {
                push("-video_size");
                push(&format!("{}x{}", r.width, r.height));
                push("-i");
                push(&format!("{display}+{},{}", r.x, r.y));
            } else {
                push("-i");
                push(&display);
            }
        }
        Platform::MacOs => {
            // avfoundation matches devices by name; "Capture screen 0" is the
            // main display. Regions are cropped on the output side.
            let screen = opts
                .display
                .clone()
                .unwrap_or_else(|| "Capture screen 0".into());
            push("-f");
            push("avfoundation");
            push("-framerate");
            push(&fps);
            push("-capture_cursor");
            push(cursor);
            push("-capture_mouse_clicks");
            push("0");
            push("-i");
            push(&format!("{screen}:none"));
        }
        Platform::Windows => {
            push("-f");
            push("gdigrab");
            push("-framerate");
            push(&fps);
            push("-draw_mouse");
            push(cursor);
            if let Some(r) = opts.region {
                push("-offset_x");
                push(&r.x.to_string());
                push("-offset_y");
                push(&r.y.to_string());
                push("-video_size");
                push(&format!("{}x{}", r.width, r.height));
            }
            push("-i");
            push(opts.display.as_deref().unwrap_or("desktop"));
        }
    }
    a
}

/// Output-side filter for the intermediate recording.
pub fn capture_filter(platform: Platform, opts: &RecordOptions) -> String {
    // yuv420p needs even dimensions.
    let even = "crop=trunc(iw/2)*2:trunc(ih/2)*2";
    match (platform, opts.region) {
        (Platform::MacOs, Some(r)) => {
            format!("crop={}:{}:{}:{},{even}", r.width, r.height, r.x, r.y)
        }
        _ => even.to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoInfo {
    pub size: Size,
    pub duration: f64,
}

pub fn probe(path: &Path) -> Result<VideoInfo> {
    #[derive(Deserialize)]
    struct Out {
        streams: Vec<Stream>,
        format: Format,
    }
    #[derive(Deserialize)]
    struct Stream {
        width: u32,
        height: u32,
    }
    #[derive(Deserialize)]
    struct Format {
        duration: Option<String>,
    }

    let out = Command::new(ffprobe_bin())
        .args(["-v", "error", "-select_streams", "v:0"])
        .args([
            "-show_entries",
            "stream=width,height:format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .stderr(Stdio::inherit())
        .output()
        .context("running ffprobe (is ffmpeg installed?)")?;
    if !out.status.success() {
        bail!("ffprobe failed on {}", path.display());
    }
    let parsed: Out = serde_json::from_slice(&out.stdout).context("parsing ffprobe output")?;
    let stream = parsed.streams.first().context("no video stream found")?;
    let duration = parsed
        .format
        .duration
        .and_then(|d| d.parse().ok())
        .unwrap_or(0.0);
    Ok(VideoInfo {
        size: Size::new(stream.width, stream.height),
        duration,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::CaptureRegion;

    fn opts(region: Option<CaptureRegion>) -> RecordOptions {
        RecordOptions {
            region,
            display: Some(":1".into()),
            ..RecordOptions::new("out.mkv")
        }
    }

    #[test]
    fn linux_region_uses_offset_in_input() {
        let r = CaptureRegion {
            x: 10,
            y: 20,
            width: 640,
            height: 480,
        };
        let a = capture_input_args(Platform::Linux, &opts(Some(r))).join(" ");
        assert!(a.contains("-video_size 640x480 -i :1+10,20"), "{a}");
    }

    #[test]
    fn macos_region_crops_on_output() {
        let r = CaptureRegion {
            x: 10,
            y: 20,
            width: 640,
            height: 480,
        };
        assert!(capture_filter(Platform::MacOs, &opts(Some(r))).starts_with("crop=640:480:10:20"));
        assert!(!capture_filter(Platform::Linux, &opts(Some(r))).contains("640"));
    }
}
