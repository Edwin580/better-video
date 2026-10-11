//! Turn an intermediate recording into a small, shareable file.

use std::{
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    cursor::CursorTrack,
    ffmpeg,
    geometry::{Rect, Size},
    native,
    overlay::ClickRings,
    render::crop_scale_rgba,
    zoom::{ZoomConfig, ZoomPlanner},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Gif,
    Mp4,
    Webm,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Gif => "gif",
            OutputFormat::Mp4 => "mp4",
            OutputFormat::Webm => "webm",
        }
    }

    /// Guess from a file extension.
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "gif" => Some(Self::Gif),
            "mp4" => Some(Self::Mp4),
            "webm" => Some(Self::Webm),
            _ => None,
        }
    }

    /// Encoder arguments, applied after the (already scaled) input.
    fn encode_args(self) -> Vec<&'static str> {
        match self {
            // One-pass palette: `split` lets palettegen see the whole clip
            // before paletteuse runs. `diff` modes keep static UI crisp and
            // the file small.
            OutputFormat::Gif => vec![
                "-filter_complex",
                "[0:v]split[a][b];[a]palettegen=stats_mode=diff[p];\
                 [b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle",
                "-loop",
                "0",
            ],
            OutputFormat::Mp4 => vec![
                "-c:v",
                "libx264",
                "-preset",
                "slow",
                "-crf",
                "28",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ],
            OutputFormat::Webm => {
                vec![
                    "-c:v",
                    "libvpx-vp9",
                    "-crf",
                    "40",
                    "-b:v",
                    "0",
                    "-row-mt",
                    "1",
                ]
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub input: PathBuf,
    pub output: PathBuf,
    pub format: OutputFormat,
    /// Output frame rate. GIFs look fine at 10–15 and stay much smaller.
    pub fps: u32,
    /// Output width in pixels (height follows aspect ratio). `None` keeps
    /// the source width.
    pub width: Option<u32>,
    /// Cursor-following zoom; needs `cursor`.
    pub zoom: Option<ZoomConfig>,
    /// Draw a ring where each click happened; needs `cursor`.
    pub click_rings: bool,
    pub cursor: Option<CursorTrack>,
    /// Seconds to cut from the start of the recording.
    pub trim_start: f64,
    /// Seconds to cut from the end of the recording.
    pub trim_end: f64,
}

impl ExportOptions {
    pub fn new(
        input: impl Into<PathBuf>,
        output: impl Into<PathBuf>,
        format: OutputFormat,
    ) -> Self {
        let fps = if format == OutputFormat::Gif { 15 } else { 30 };
        Self {
            input: input.into(),
            output: output.into(),
            format,
            fps,
            width: Some(1280),
            zoom: None,
            click_rings: false,
            cursor: None,
            trim_start: 0.0,
            trim_end: 0.0,
        }
    }
}

pub fn export(opts: &ExportOptions) -> Result<PathBuf> {
    let wants_cursor = opts.zoom.is_some() || opts.click_rings;
    let track = match &opts.cursor {
        Some(track) if wants_cursor && !track.is_empty() => {
            let mut track = track.clone();
            track.shift_earlier(opts.trim_start);
            Some(track)
        }
        _ => None,
    };
    if bv_engine::raw::is_raw(&opts.input) {
        native::export(opts, track.as_ref())?;
        return Ok(opts.output.clone());
    }

    let info = ffmpeg::probe(&opts.input)?;
    let out_size = info.size.fit_width(opts.width.unwrap_or(info.size.width));
    let input = input_args(opts, info.duration)?;
    match &track {
        Some(track) => export_rendered(opts, &input, track, info.size, out_size)?,
        None => export_plain(opts, &input, out_size)?,
    }
    Ok(opts.output.clone())
}

/// `-i` plus any seek/length options needed for trimming.
fn input_args(opts: &ExportOptions, duration: f64) -> Result<Vec<String>> {
    let (start, end) = (opts.trim_start.max(0.0), opts.trim_end.max(0.0));
    let mut args = Vec::new();
    if start > 0.0 || end > 0.0 {
        let keep = trimmed_length(duration, start, end)?;
        args.extend(["-ss".into(), format!("{start:.3}")]);
        args.extend(["-t".into(), format!("{keep:.3}")]);
    }
    args.push("-i".into());
    args.push(opts.input.to_string_lossy().into_owned());
    Ok(args)
}

pub(crate) fn trimmed_length(duration: f64, start: f64, end: f64) -> Result<f64> {
    let keep = duration - start - end;
    if keep <= 0.0 {
        bail!("trimming {start:.1}s + {end:.1}s leaves nothing of a {duration:.1}s recording");
    }
    Ok(keep)
}

/// No zoom: a single ffmpeg invocation does everything.
fn export_plain(opts: &ExportOptions, input: &[String], out: Size) -> Result<()> {
    let scale = format!(
        "fps={},scale={}:{}:flags=lanczos",
        opts.fps, out.width, out.height
    );
    let mut cmd = ffmpeg::ffmpeg();
    cmd.arg("-y").args(input);
    match opts.format {
        // GIF already uses -filter_complex; prepend the scale chain to it.
        OutputFormat::Gif => {
            let fc = format!(
                "[0:v]{scale},split[a][b];[a]palettegen=stats_mode=diff[p];\
                 [b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"
            );
            cmd.args(["-filter_complex", &fc, "-loop", "0"]);
        }
        f => {
            cmd.args(["-vf", &scale]).args(f.encode_args());
        }
    }
    let status = cmd
        .arg("-an")
        .arg(&opts.output)
        .status()
        .context("running ffmpeg")?;
    if !status.success() {
        bail!("ffmpeg export failed ({status})");
    }
    Ok(())
}

/// Zoom / click rings: ffmpeg decodes to raw RGBA, we pick a viewport per
/// frame, crop/scale it and draw overlays, then pipe the result into a
/// second ffmpeg to encode.
fn export_rendered(
    opts: &ExportOptions,
    input: &[String],
    track: &CursorTrack,
    src: Size,
    out: Size,
) -> Result<()> {
    let mut decoder = ffmpeg::ffmpeg()
        .args(input)
        .args(["-vf", &format!("fps={}", opts.fps)])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .context("starting ffmpeg decoder")?;

    let mut encoder = spawn_encoder(opts, out)?;

    let result = pump_frames(&mut decoder, &mut encoder, opts, track, src, out);
    // Close the encoder's stdin so it can finish, even on error.
    drop(encoder.stdin.take());
    let enc_status = encoder.wait()?;
    let dec_status = decoder.wait()?;
    result?;
    if !dec_status.success() {
        bail!("ffmpeg decoder failed ({dec_status})");
    }
    if !enc_status.success() {
        bail!("ffmpeg encoder failed ({enc_status})");
    }
    Ok(())
}

/// ffmpeg reading raw RGBA frames of size `out` on stdin and encoding them
/// in `opts.format`.
pub(crate) fn spawn_encoder(opts: &ExportOptions, out: Size) -> Result<Child> {
    ffmpeg::ffmpeg()
        .arg("-y")
        .args(["-f", "rawvideo", "-pix_fmt", "rgba"])
        .args(["-s", &format!("{}x{}", out.width, out.height)])
        .args(["-r", &opts.fps.to_string()])
        .args(["-i", "-"])
        .args(opts.format.encode_args())
        .arg(&opts.output)
        .stdin(Stdio::piped())
        .spawn()
        .context("starting ffmpeg encoder (MP4/WebM export needs ffmpeg)")
}

fn pump_frames(
    decoder: &mut Child,
    encoder: &mut Child,
    opts: &ExportOptions,
    track: &CursorTrack,
    src: Size,
    out: Size,
) -> Result<()> {
    let mut reader = decoder.stdout.take().context("decoder stdout")?;
    let mut writer = BufWriter::new(encoder.stdin.take().context("encoder stdin")?);
    let mut planner = opts.zoom.map(|z| ZoomPlanner::new(z, track, src));
    let rings = opts.click_rings.then(|| ClickRings::new(track));

    let mut frame = vec![0u8; src.width as usize * src.height as usize * 4];
    let mut scaled = vec![0u8; out.width as usize * out.height as usize * 4];
    let mut index: u64 = 0;
    while read_frame(&mut reader, &mut frame)? {
        let t = index as f64 / opts.fps as f64;
        let view = planner.as_mut().map_or(Rect::full(src), |p| p.next(t));
        crop_scale_rgba(&frame, src, view, &mut scaled, out);
        if let Some(rings) = &rings {
            rings.draw(&mut scaled, out, view, t);
        }
        writer
            .write_all(&scaled)
            .context("writing to ffmpeg encoder")?;
        index += 1;
    }
    writer.flush()?;
    Ok(())
}

/// Fill `buf` with exactly one frame; `Ok(false)` on clean end of stream.
fn read_frame(r: &mut impl Read, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..])? {
            0 if filled == 0 => return Ok(false),
            0 => bail!("truncated frame from decoder"),
            n => filled += n,
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_args_only_when_trimming() {
        let mut opts = ExportOptions::new("in.mkv", "out.gif", OutputFormat::Gif);
        assert_eq!(input_args(&opts, 10.0).unwrap(), ["-i", "in.mkv"]);
        opts.trim_start = 1.5;
        opts.trim_end = 2.0;
        assert_eq!(
            input_args(&opts, 10.0).unwrap(),
            ["-ss", "1.500", "-t", "6.500", "-i", "in.mkv"]
        );
    }

    #[test]
    fn rejects_trimming_everything() {
        assert!(trimmed_length(3.0, 2.0, 1.0).is_err());
    }
}
