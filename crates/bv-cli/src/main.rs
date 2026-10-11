mod tracker;

use std::{
    io::{self, BufRead, IsTerminal},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use bv_core::{
    export, ffmpeg, zoom::ZoomTrigger, CaptureRegion, CursorTrack, ExportOptions, OutputFormat,
    RecordOptions, Recording, ZoomConfig,
};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::tracker::CursorTracker;

/// Record your screen and get a small GIF/MP4 you can drop into a PR.
#[derive(Parser)]
#[command(name = "bv", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record the screen, then export. Stop with Enter or Ctrl+C.
    Record(RecordArgs),
    /// Re-export a raw recording (e.g. with different zoom settings).
    Export(ExportCmd),
    /// Check that dependencies are installed.
    Doctor,
}

#[derive(Args)]
struct RecordArgs {
    /// Output file. Defaults to ./bv-<timestamp>.<format>.
    output: Option<PathBuf>,
    /// Capture only this region: WIDTHxHEIGHT+X+Y.
    #[arg(long)]
    region: Option<CaptureRegion>,
    /// Screen to capture (X11 display, avfoundation device or gdigrab target).
    #[arg(long)]
    display: Option<String>,
    /// Stop automatically after this many seconds.
    #[arg(long, short = 'd')]
    duration: Option<f64>,
    /// Capture frame rate.
    #[arg(long, default_value_t = 30)]
    capture_fps: u32,
    /// Keep the raw recording + cursor track next to the output so you can
    /// re-run `bv export` later.
    #[arg(long)]
    keep_raw: bool,
    #[command(flatten)]
    export: ExportArgs,
}

#[derive(Args)]
struct ExportCmd {
    /// Raw recording produced by `bv record --keep-raw`.
    input: PathBuf,
    /// Output file. Defaults to the input name with the format's extension.
    #[arg(long, short)]
    output: Option<PathBuf>,
    /// Cursor track JSON. Defaults to <input>.cursor.json if it exists.
    #[arg(long)]
    cursor: Option<PathBuf>,
    #[command(flatten)]
    export: ExportArgs,
}

#[derive(Args)]
struct ExportArgs {
    /// Output format (inferred from the output extension if omitted).
    #[arg(long, short)]
    format: Option<Format>,
    /// Shorthand for --format gif.
    #[arg(long, conflicts_with = "format")]
    gif: bool,
    /// Output width in pixels (height keeps aspect ratio).
    #[arg(long, short, default_value_t = 1280)]
    width: u32,
    /// Output frame rate [default: 15 for GIF, 30 otherwise].
    #[arg(long)]
    fps: Option<u32>,
    /// Zoom in on the cursor when you click.
    #[arg(long, short)]
    zoom: bool,
    /// Stay zoomed in and follow the cursor the whole time.
    #[arg(long)]
    zoom_follow: bool,
    /// Zoom magnification.
    #[arg(long, default_value_t = 2.0)]
    zoom_level: f64,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Gif,
    Mp4,
    Webm,
}

impl From<Format> for OutputFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Gif => OutputFormat::Gif,
            Format::Mp4 => OutputFormat::Mp4,
            Format::Webm => OutputFormat::Webm,
        }
    }
}

impl ExportArgs {
    fn format(&self, output: Option<&Path>) -> OutputFormat {
        if self.gif {
            return OutputFormat::Gif;
        }
        self.format
            .map(Into::into)
            .or_else(|| output.and_then(OutputFormat::from_path))
            .unwrap_or(OutputFormat::Gif)
    }

    fn zoom(&self) -> Option<ZoomConfig> {
        (self.zoom || self.zoom_follow).then(|| ZoomConfig {
            level: self.zoom_level,
            trigger: if self.zoom_follow {
                ZoomTrigger::Always
            } else {
                ZoomTrigger::Click
            },
            ..ZoomConfig::default()
        })
    }

    fn options(&self, input: PathBuf, output: PathBuf, format: OutputFormat) -> ExportOptions {
        let mut opts = ExportOptions::new(input, output, format);
        opts.width = Some(self.width);
        if let Some(fps) = self.fps {
            opts.fps = fps;
        }
        opts.zoom = self.zoom();
        opts
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Record(args) => record(args),
        Command::Export(args) => export_cmd(args),
        Command::Doctor => doctor(),
    }
}

fn record(args: RecordArgs) -> Result<()> {
    let format = args.export.format(args.output.as_deref());
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| default_output(format));
    let raw = if args.keep_raw {
        output.with_extension("raw.mkv")
    } else {
        let dir = std::env::temp_dir().join("better-video");
        std::fs::create_dir_all(&dir)?;
        dir.join(
            output
                .with_extension("raw.mkv")
                .file_name()
                .context("bad output path")?,
        )
    };

    let opts = RecordOptions {
        fps: args.capture_fps,
        region: args.region,
        display: args.display.clone(),
        max_duration: args.duration.map(Duration::from_secs_f64),
        ..RecordOptions::new(&raw)
    };

    let stop = stop_signal()?;
    let recording = Recording::start(&opts)?;
    let tracker = if args.export.zoom().is_some() {
        let t = CursorTracker::start(recording.started(), args.region);
        if t.is_none() {
            eprintln!("warning: cursor tracking unavailable; exporting without zoom");
        }
        t
    } else {
        None
    };

    eprintln!("● Recording… press Enter or Ctrl+C to stop");
    let mut recording = recording;
    while !stop.load(Ordering::Relaxed) && !recording.finished() {
        thread::sleep(Duration::from_millis(50));
    }
    let secs = recording.elapsed().as_secs_f64();
    let mut track = tracker.map(CursorTracker::finish);
    let recorded = recording.stop()?;
    let raw = recorded.path;
    if let Some(t) = &mut track {
        t.shift_earlier(recorded.first_frame_delay);
    }
    eprintln!("■ Stopped after {secs:.1}s, exporting…");

    if args.keep_raw {
        if let Some(t) = &track {
            t.save(&cursor_sidecar(&raw))?;
        }
    }

    let mut eopts = args.export.options(raw.clone(), output, format);
    eopts.cursor = track;
    let out = export(&eopts)?;
    if !args.keep_raw {
        let _ = std::fs::remove_file(&raw);
    }
    report(&out);
    Ok(())
}

fn export_cmd(args: ExportCmd) -> Result<()> {
    let format = args.export.format(args.output.as_deref());
    let output = args.output.clone().unwrap_or_else(|| {
        let stem = args.input.with_extension("");
        // "foo.raw.mkv" -> "foo.gif"
        stem.with_extension("").with_extension(format.extension())
    });
    let mut opts = args.export.options(args.input.clone(), output, format);
    if opts.zoom.is_some() {
        let path = args
            .cursor
            .clone()
            .unwrap_or_else(|| cursor_sidecar(&args.input));
        opts.cursor = Some(CursorTrack::load(&path).with_context(|| {
            "zoom needs a cursor track; pass --cursor or record with --keep-raw"
        })?);
    }
    report(&export(&opts)?);
    Ok(())
}

fn doctor() -> Result<()> {
    match ffmpeg::version() {
        Some(v) => println!("✓ {v}"),
        None => println!("✗ ffmpeg not found — install it or set BV_FFMPEG"),
    }
    let cursor = CursorTracker::start(Instant::now(), None).map(CursorTracker::finish);
    match cursor {
        Some(_) => println!("✓ cursor tracking available (zoom supported)"),
        None => println!("✗ cursor tracking unavailable (zoom disabled)"),
    }
    println!("  platform: {:?}", ffmpeg::Platform::current());
    Ok(())
}

/// Flag set by Ctrl+C or by pressing Enter (when attached to a terminal).
fn stop_signal() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    ctrlc::set_handler(move || s.store(true, Ordering::Relaxed))
        .context("installing Ctrl+C handler")?;
    if io::stdin().is_terminal() {
        let s = stop.clone();
        thread::spawn(move || {
            let _ = io::stdin().lock().lines().next();
            s.store(true, Ordering::Relaxed);
        });
    }
    Ok(stop)
}

fn default_output(format: OutputFormat) -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    PathBuf::from(format!("bv-{ts}.{}", format.extension()))
}

fn cursor_sidecar(raw: &Path) -> PathBuf {
    raw.with_extension("cursor.json")
}

fn report(out: &Path) {
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    println!("{} ({:.1} MB)", out.display(), size as f64 / 1_048_576.0);
    if size > 10 * 1_048_576 {
        eprintln!("note: GitHub limits PR attachments to 10 MB; try --width 960, --fps 10 or --format mp4");
    }
}
