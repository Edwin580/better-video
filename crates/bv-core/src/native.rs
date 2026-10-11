//! Export from a native `.bvraw` recording. GIFs are encoded entirely by
//! bv-engine; MP4/WebM frames are piped to ffmpeg.

use std::{
    fs::File,
    io::{BufWriter, Write},
};

use anyhow::{bail, Context, Result};
use bv_engine::{GifEncoder, PaletteBuilder, RawReader};

use crate::{
    cursor::CursorTrack,
    export::{spawn_encoder, trimmed_length, ExportOptions, OutputFormat},
    geometry::{Rect, Size},
    overlay::ClickRings,
    render::crop_scale_rgba_smooth,
    zoom::ZoomPlanner,
};

/// `track` is already shifted for `trim_start`, and `None` unless zoom or
/// click rings need it.
pub fn export(opts: &ExportOptions, track: Option<&CursorTrack>) -> Result<()> {
    let reader = open(opts)?;
    let src = Size::new(reader.width(), reader.height());
    let out = src.fit_width(opts.width.unwrap_or(src.width));
    let keep = trimmed_length(
        reader.duration(),
        opts.trim_start.max(0.0),
        opts.trim_end.max(0.0),
    )?;
    let job = Job {
        opts,
        track,
        src,
        out,
        keep,
    };
    match opts.format {
        OutputFormat::Gif => job.gif(),
        _ => job.video(),
    }
}

fn open(opts: &ExportOptions) -> Result<RawReader> {
    RawReader::open(&opts.input).with_context(|| format!("reading {}", opts.input.display()))
}

struct Job<'a> {
    opts: &'a ExportOptions,
    track: Option<&'a CursorTrack>,
    src: Size,
    out: Size,
    /// Seconds of recording to export, after trimming.
    keep: f64,
}

impl Job<'_> {
    /// Two passes over the clip: the first collects colours for the
    /// palette, the second maps and encodes.
    fn gif(&self) -> Result<()> {
        if self.out.width > u16::MAX as u32 || self.out.height > u16::MAX as u32 {
            bail!("GIF can't be larger than 65535 pixels per side");
        }
        let mut colors = PaletteBuilder::new();
        self.render(|frame, changed| {
            if changed {
                colors.add(frame);
            }
            Ok(())
        })?;
        let mut palette = colors.build();

        let file = File::create(&self.opts.output)
            .with_context(|| format!("creating {}", self.opts.output.display()))?;
        let mut gif = GifEncoder::new(
            BufWriter::new(file),
            self.out.width as u16,
            self.out.height as u16,
            palette.colors(),
        )?;
        let mut indices = vec![0u8; self.out.width as usize * self.out.height as usize];
        let secs = 1.0 / self.opts.fps as f64;
        self.render(|frame, changed| {
            if changed {
                palette.map(frame, &mut indices);
            }
            Ok(gif.push(&indices, secs)?)
        })?;
        gif.finish()?;
        Ok(())
    }

    fn video(&self) -> Result<()> {
        let mut encoder = spawn_encoder(self.opts, self.out)?;
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
    /// whether it differs from the previous one (unchanged frames aren't
    /// re-rendered, which is most of them in a typical screen recording).
    fn render(&self, mut sink: impl FnMut(&[u8], bool) -> Result<()>) -> Result<()> {
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
        let mut scaled = vec![0u8; self.out.width as usize * self.out.height as usize * 4];
        let mut scratch = Vec::new();
        let mut last_view = None;
        let mut rings_were_active = false;

        for i in 0..frames {
            let t = i as f64 / fps;
            let new_pixels = reader.advance_to(start + t)?;
            let view = planner.as_mut().map_or(Rect::full(self.src), |p| p.next(t));
            let rings_active = rings.as_ref().is_some_and(|r| r.active_at(t));
            // A ring that just finished still has to be erased.
            let changed = i == 0
                || new_pixels
                || last_view != Some(view)
                || rings_active
                || rings_were_active;
            if changed {
                crop_scale_rgba_smooth(
                    reader.frame(),
                    self.src,
                    view,
                    &mut scaled,
                    self.out,
                    &mut scratch,
                );
                if let Some(r) = &rings {
                    r.draw(&mut scaled, self.out, view, t);
                }
            }
            last_view = Some(view);
            rings_were_active = rings_active;
            sink(&scaled, changed)?;
        }
        Ok(())
    }
}
