//! Export from a native `.bvraw` recording. GIFs are encoded entirely by
//! bv-engine and MP4 by Windows' Media Foundation; WebM (and MP4 where
//! Media Foundation is missing) is piped to ffmpeg.

use std::{
    fs::File,
    io::{BufWriter, Write},
    ops::Range,
};

use anyhow::{bail, Context, Result};
use bv_engine::{GifEncoder, Mp4Encoder, PaletteBuilder, RawReader};

use crate::{
    cursor::CursorTrack,
    export::{spawn_encoder, trimmed_length, ExportOptions, OutputFormat},
    geometry::{Rect, Size},
    overlay::ClickRings,
    render::{affected_rows, crop_scale_rgba_smooth},
    style::{Frame, Layout},
    zoom::ZoomPlanner,
};

/// Ordered dither amplitude (levels) for the static background of styled
/// GIFs: about one palette step in a smooth gradient.
const GIF_DITHER: f64 = 10.0;

/// `track` is already shifted for `trim_start`, and `None` unless zoom or
/// click rings need it.
/// Returns the output size and length.
pub fn export(opts: &ExportOptions, track: Option<&CursorTrack>) -> Result<(Size, f64)> {
    let reader = open(opts)?;
    let src = Size::new(reader.width(), reader.height());
    let layout = Layout::new(opts.style.as_ref(), src, opts.width);
    let keep = trimmed_length(
        reader.duration(),
        opts.trim_start.max(0.0),
        opts.trim_end.max(0.0),
    )?;
    let job = Job {
        opts,
        track,
        src,
        layout,
        keep,
    };
    match opts.format {
        OutputFormat::Gif => job.gif()?,
        _ => job.video()?,
    }
    Ok((layout.canvas, keep))
}

fn open(opts: &ExportOptions) -> Result<RawReader> {
    RawReader::open(&opts.input).with_context(|| format!("reading {}", opts.input.display()))
}

struct Job<'a> {
    opts: &'a ExportOptions,
    track: Option<&'a CursorTrack>,
    src: Size,
    /// The output frame, and where the recording sits in it.
    layout: Layout,
    /// Seconds of recording to export, after trimming.
    keep: f64,
}

impl Job<'_> {
    /// Two passes over the clip: the first collects colours for the
    /// palette, the second maps and encodes. Both only look at the rows
    /// that changed.
    fn gif(&self) -> Result<()> {
        if self.layout.canvas.width > u16::MAX as u32 || self.layout.canvas.height > u16::MAX as u32
        {
            bail!("GIF can't be larger than 65535 pixels per side");
        }
        let stride = self.layout.canvas.width as usize * 4;
        let mut colors = PaletteBuilder::new();
        self.render(|frame, rows| {
            if let Some(rows) = rows {
                colors.add(&frame[rows.start * stride..rows.end * stride]);
            }
            Ok(())
        })?;
        let mut palette = colors.build();

        let file = File::create(&self.opts.output)
            .with_context(|| format!("creating {}", self.opts.output.display()))?;
        let mut gif = GifEncoder::new(
            BufWriter::new(file),
            self.layout.canvas.width as u16,
            self.layout.canvas.height as u16,
            palette.colors(),
        )?;
        let width = self.layout.canvas.width as usize;
        let mut indices = vec![0u8; width * self.layout.canvas.height as usize];
        let secs = 1.0 / self.opts.fps as f64;
        self.render(|frame, rows| {
            if let Some(rows) = rows {
                palette.map(
                    &frame[rows.start * stride..rows.end * stride],
                    &mut indices[rows.start * width..rows.end * width],
                );
            }
            Ok(gif.push(&indices, secs)?)
        })?;
        gif.finish()?;
        Ok(())
    }

    fn video(&self) -> Result<()> {
        // MP4 with Windows' own H.264 encoder: no ffmpeg needed. Where
        // Media Foundation is missing (Windows "N" editions), use ffmpeg.
        if self.opts.format == OutputFormat::Mp4 {
            match Mp4Encoder::create(
                &self.opts.output,
                self.layout.canvas.width,
                self.layout.canvas.height,
                self.opts.fps,
            ) {
                Ok(mut mp4) => {
                    self.render(|frame, _| Ok(mp4.push(frame)?))?;
                    return Ok(mp4.finish()?);
                }
                Err(e) if cfg!(windows) => {
                    eprintln!("note: built-in MP4 encoder unavailable ({e}); trying ffmpeg");
                }
                Err(_) => {}
            }
        }
        let mut encoder = spawn_encoder(self.opts, self.layout.canvas)?;
        let mut stdin = BufWriter::new(encoder.stdin.take().context("encoder stdin")?);
        let result =
            self.render(|frame, _| stdin.write_all(frame).context("writing to ffmpeg encoder"));
        let flushed = stdin.flush();
        drop(stdin);
        let status = encoder.wait()?;
        result?;
        flushed?;
        if !status.success() {
            bail!("ffmpeg encoder failed ({status})");
        }
        Ok(())
    }

    /// Produce every output frame in order. `sink` gets the RGBA frame and
    /// the rows that differ from the previous frame (`None` if none do).
    /// Only those rows are re-rendered: in a typical screen recording most
    /// frames change a little or not at all.
    fn render(
        &self,
        mut sink: impl FnMut(&[u8], Option<Range<usize>>) -> Result<()>,
    ) -> Result<()> {
        let opts = self.opts;
        let mut reader = open(opts)?;
        let mut planner = self
            .track
            .zip(opts.zoom)
            .map(|(t, z)| ZoomPlanner::new(z, t, self.src));
        let rings = self.track.filter(|_| opts.click_rings).map(ClickRings::new);

        let fps = opts.fps.max(1) as f64;
        let start = opts.trim_start.max(0.0);
        let frames = ((self.keep * fps).round() as u64).max(1);
        let frame = opts.style.as_ref().map(|s| {
            let mut frame = Frame::new(s, self.layout);
            // Our GIF palette mapping doesn't dither; the background must.
            if opts.format == OutputFormat::Gif {
                frame.dither(GIF_DITHER);
            }
            frame
        });
        let mut canvas = frame.as_ref().map(Frame::blank);
        // The recording's own size inside the frame.
        let out = self.layout.content;
        let all = 0..out.height as usize;
        let mut scaled = vec![0u8; out.width as usize * out.height as usize * 4];
        let mut scratch = Vec::new();
        let mut last_view = None;
        let mut rings_were_active = false;

        for i in 0..frames {
            let t = i as f64 / fps;
            let new_rows = reader.advance_to(start + t)?;
            let view = planner.as_mut().map_or(Rect::full(self.src), |p| p.next(t));
            let rings_active = rings.as_ref().is_some_and(|r| r.active_at(t));
            // Redraw everything when the camera moved or a ring is (or was
            // just) on screen; otherwise only the rows under new pixels.
            let rows = if i == 0 || last_view != Some(view) || rings_active || rings_were_active {
                Some(all.clone())
            } else {
                new_rows
                    .map(|r| affected_rows(r, view, out))
                    .filter(|r| !r.is_empty())
            };
            if let Some(rows) = &rows {
                crop_scale_rgba_smooth(
                    reader.frame(),
                    self.src,
                    view,
                    &mut scaled,
                    out,
                    rows.clone(),
                    &mut scratch,
                );
                if let Some(r) = &rings {
                    r.draw(&mut scaled, out, view, t);
                }
            }
            last_view = Some(view);
            rings_were_active = rings_active;
            match (&frame, &mut canvas) {
                (Some(f), Some(canvas)) => {
                    if let Some(rows) = &rows {
                        f.compose(&scaled, canvas, rows.clone());
                    }
                    // The first frame is the whole canvas, padding included.
                    let rows = match i {
                        0 => Some(0..self.layout.canvas.height as usize),
                        _ => rows.map(|r| f.canvas_rows(r)),
                    };
                    sink(canvas, rows)?;
                }
                _ => sink(&scaled, rows)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use bv_engine::RawWriter;

    use super::*;
    use crate::style::StylePreset;

    /// A 64×48 recording where a small square moves each frame.
    fn recording(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("bv-{name}-{}.bvraw", std::process::id()));
        let mut w = RawWriter::create(&path, 64, 48).unwrap();
        for i in 0..4 {
            let mut f = vec![200u8; 64 * 48 * 4];
            for y in 20..24 {
                for x in 10 + i * 8..14 + i * 8 {
                    f[(y * 64 + x) * 4..][..3].copy_from_slice(&[0, 0, 255]);
                }
            }
            w.push(i as f64 * 0.1, &f).unwrap();
        }
        w.finish(0.4).unwrap();
        path
    }

    #[test]
    fn styled_frames_only_change_inside_the_recording() {
        let input = recording("styled");
        let mut opts = ExportOptions::new(&input, "unused.gif", OutputFormat::Gif);
        opts.fps = 10;
        opts.style = Some(StylePreset::Gradient.style());
        let src = Size::new(64, 48);
        let layout = Layout::new(opts.style.as_ref(), src, opts.width);
        assert!(layout.x > 0);
        let job = Job {
            opts: &opts,
            track: None,
            src,
            layout,
            keep: 0.4,
        };
        let mut frames: Vec<(Vec<u8>, Option<Range<usize>>)> = Vec::new();
        job.render(|f, rows| {
            frames.push((f.to_vec(), rows));
            Ok(())
        })
        .unwrap();
        std::fs::remove_file(&input).ok();

        assert_eq!(frames.len(), 4);
        assert_eq!(frames[0].1, Some(0..layout.canvas.height as usize));
        let stride = layout.canvas.width as usize * 4;
        let (x0, x1) = (
            layout.x as usize,
            (layout.x + layout.content.width) as usize,
        );
        for pair in frames.windows(2) {
            let (a, b) = (&pair[0].0, &pair[1].0);
            let rows = pair[1].1.clone().expect("the square moved");
            assert!(rows.start >= layout.y as usize);
            assert!(rows.end <= (layout.y + layout.content.height) as usize);
            for (i, (p, q)) in a.iter().zip(b).enumerate() {
                let (x, y) = ((i % stride) / 4, i / stride);
                if p != q {
                    assert!(rows.contains(&y) && (x0..x1).contains(&x), "({x}, {y})");
                }
            }
        }
        // And the content really changed.
        assert_ne!(frames[1].0, frames[2].0);
    }
}
