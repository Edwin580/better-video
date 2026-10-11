mod clipboard;
mod hotkey;
mod tracker;
#[cfg(feature = "tray")]
mod tray;

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
    export, ffmpeg, monitor, zoom::ZoomTrigger, CaptureRegion, CursorTrack, ExportOptions,
    MonitorChoice, OutputFormat, RecordOptions, Recording, ZoomConfig,
};
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::{
    hotkey::{EdgeDetector, Hotkey},
    tracker::CursorTracker,
};

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
    /// Wait in the background; a global hotkey starts and stops recording.
    Hotkey(HotkeyArgs),
    /// Menu bar / system tray app with the same hotkey.
    #[cfg(feature = "tray")]
    Tray(HotkeyArgs),
    /// Re-export a raw recording (e.g. with different zoom settings).
    Export(ExportCmd),
    /// List connected monitors (for --monitor).
    Monitors,
    /// Check that dependencies are installed.
    Doctor,
}

#[derive(Args)]
struct RecordArgs {
    /// Output file. Defaults to ./bv-<timestamp>.<format>.
    output: Option<PathBuf>,
    #[command(flatten)]
    capture: CaptureArgs,
}

#[derive(Args)]
struct HotkeyArgs {
    /// Key combination that starts and stops recording.
    #[arg(long, default_value = "ctrl+alt+r")]
    key: String,
    /// Folder to save recordings in [default: current folder for `hotkey`,
    /// ~/Videos/better-video (~/Movies on macOS) for `tray`].
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Don't copy each recording to the clipboard.
    #[arg(long)]
    no_copy: bool,
    #[command(flatten)]
    capture: CaptureArgs,
}

#[derive(Args, Clone)]
struct CaptureArgs {
    /// Capture only this region: WIDTHxHEIGHT+X+Y (offsets may be negative,
    /// e.g. 1920x1080-1920+0 for a monitor left of the main one).
    #[arg(long, conflicts_with = "monitor")]
    region: Option<CaptureRegion>,
    /// Screen to record: a number from `bv monitors`, `primary` or `all`
    /// [default on Windows: primary].
    #[arg(long, short = 'm')]
    monitor: Option<MonitorChoice>,
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

#[derive(Args, Clone)]
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
    /// Copy the exported file to the clipboard.
    #[arg(long, short = 'c')]
    copy: bool,
    /// Draw a ring wherever you click.
    #[arg(long)]
    clicks: bool,
    /// Zoom magnification.
    #[arg(long, default_value_t = 2.0)]
    zoom_level: f64,
    /// Seconds to cut from the start (e.g. reaching for the mouse).
    #[arg(long, default_value_t = 0.0, value_name = "SECS")]
    trim_start: f64,
    /// Seconds to cut from the end (e.g. pressing the stop key).
    #[arg(long, default_value_t = 0.0, value_name = "SECS")]
    trim_end: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
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

    fn needs_cursor(&self) -> bool {
        self.zoom().is_some() || self.clicks
    }

    fn options(&self, input: PathBuf, output: PathBuf, format: OutputFormat) -> ExportOptions {
        let mut opts = ExportOptions::new(input, output, format);
        opts.width = Some(self.width);
        if let Some(fps) = self.fps {
            opts.fps = fps;
        }
        opts.zoom = self.zoom();
        opts.click_rings = self.clicks;
        opts.trim_start = self.trim_start;
        opts.trim_end = self.trim_end;
        opts
    }
}

fn main() -> Result<()> {
    // Physical pixels everywhere, so monitor bounds, capture offsets and
    // cursor positions agree on scaled displays.
    monitor::set_dpi_aware();
    match Cli::parse().command {
        Command::Record(args) => record(args),
        Command::Hotkey(args) => hotkey_mode(args),
        #[cfg(feature = "tray")]
        Command::Tray(args) => tray::run(args),
        Command::Export(args) => export_cmd(args),
        Command::Monitors => list_monitors(),
        Command::Doctor => doctor(),
    }
}

impl CaptureArgs {
    /// The region to record: `--region`, else `--monitor`, else (on Windows,
    /// where the desktop spans every screen) the primary monitor.
    fn capture_region(&self) -> Result<Option<CaptureRegion>> {
        if self.region.is_some() || self.display.is_some() {
            return Ok(self.region);
        }
        let choice = match self.monitor {
            Some(c) => c,
            None if cfg!(windows) => MonitorChoice::Primary,
            None => return Ok(None),
        };
        choice
            .region(&monitor::monitors())
            .map_err(anyhow::Error::msg)
    }
}

fn list_monitors() -> Result<()> {
    let list = monitor::monitors();
    if list.is_empty() {
        println!("Monitor listing isn't supported on this platform yet; use --region.");
    }
    for (i, m) in list.iter().enumerate() {
        println!("{}: {m}  (--region {})", i + 1, m.region);
    }
    Ok(())
}

fn record(args: RecordArgs) -> Result<()> {
    let format = args.capture.export.format(args.output.as_deref());
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| default_output(format));
    let stop = stop_signal(true)?;
    let mut session = Session::start(&args.capture, output, format)?;
    eprintln!("● Recording… press Enter or Ctrl+C to stop");
    while !stop.load(Ordering::Relaxed) && !session.finished() {
        thread::sleep(Duration::from_millis(50));
    }
    session.finish(&args.capture).map(drop)
}

fn hotkey_mode(mut args: HotkeyArgs) -> Result<()> {
    let key: Hotkey = args.key.parse().map_err(anyhow::Error::msg)?;
    let mut keys = hotkey::keyboard().context(
        "keyboard access unavailable (needs X11 on Linux; Input Monitoring permission on macOS)",
    )?;
    let dir = args.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;
    args.capture.export.copy |= !args.no_copy;
    let format = args.capture.export.format(None);
    let quit = stop_signal(false)?;
    let mut edge = EdgeDetector::default();
    let mut session: Option<Session> = None;

    eprintln!("Press {} to start/stop recording, Ctrl+C to quit", args.key);
    loop {
        let pressed = edge.pressed(key.held(&keys()));
        let quitting = quit.load(Ordering::Relaxed);
        let ended = session.as_mut().is_some_and(Session::finished);
        match &session {
            None if quitting => return Ok(()),
            None if pressed => {
                let output = dir.join(default_output(format));
                match Session::start(&args.capture, output, format) {
                    Ok(s) => {
                        eprintln!("● Recording… press {} to stop", args.key);
                        session = Some(s);
                    }
                    Err(e) => eprintln!("error: {e:#}"),
                }
            }
            Some(_) if pressed || quitting || ended => {
                if let Some(s) = session.take() {
                    if let Err(e) = s.finish(&args.capture) {
                        eprintln!("error: {e:#}");
                    }
                }
                if quitting {
                    return Ok(());
                }
                eprintln!("Ready. Press {} to record again", args.key);
            }
            _ => {}
        }
        thread::sleep(hotkey::POLL_INTERVAL);
    }
}

/// One recording from start to exported file.
struct Session {
    recording: Recording,
    tracker: Option<CursorTracker>,
    output: PathBuf,
    format: OutputFormat,
}

impl Session {
    fn start(args: &CaptureArgs, output: PathBuf, format: OutputFormat) -> Result<Self> {
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
        let region = args.capture_region()?;
        let opts = RecordOptions {
            fps: args.capture_fps,
            region,
            display: args.display.clone(),
            max_duration: args.duration.map(Duration::from_secs_f64),
            ..RecordOptions::new(&raw)
        };

        let recording = Recording::start(&opts)?;
        let tracker = if args.export.needs_cursor() {
            let t = CursorTracker::start(recording.started(), region);
            if t.is_none() {
                eprintln!("warning: cursor tracking unavailable; exporting without zoom/clicks");
            }
            t
        } else {
            None
        };
        Ok(Self {
            recording,
            tracker,
            output,
            format,
        })
    }

    fn finished(&mut self) -> bool {
        self.recording.finished()
    }

    /// Stop recording, export, and report the result.
    fn finish(self, args: &CaptureArgs) -> Result<PathBuf> {
        let secs = self.recording.elapsed().as_secs_f64();
        let mut track = self.tracker.map(CursorTracker::finish);
        let recorded = self.recording.stop()?;
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

        let mut eopts = args.export.options(raw.clone(), self.output, self.format);
        eopts.cursor = track;
        let result = export(&eopts);
        if !args.keep_raw {
            let _ = std::fs::remove_file(&raw);
        }
        let out = result?;
        report(&out, args.export.copy);
        Ok(out)
    }
}

fn export_cmd(args: ExportCmd) -> Result<()> {
    let format = args.export.format(args.output.as_deref());
    let output = args.output.clone().unwrap_or_else(|| {
        let stem = args.input.with_extension("");
        // "foo.raw.mkv" -> "foo.gif"
        stem.with_extension("").with_extension(format.extension())
    });
    let mut opts = args.export.options(args.input.clone(), output, format);
    if args.export.needs_cursor() {
        let path = args
            .cursor
            .clone()
            .unwrap_or_else(|| cursor_sidecar(&args.input));
        opts.cursor = Some(CursorTrack::load(&path).with_context(|| {
            "zoom/clicks need a cursor track; pass --cursor or record with --keep-raw"
        })?);
    }
    report(&export(&opts)?, args.export.copy);
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

/// Flag set by Ctrl+C, or by pressing Enter (when `enter` and attached to
/// a terminal).
fn stop_signal(enter: bool) -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    ctrlc::set_handler(move || s.store(true, Ordering::Relaxed))
        .context("installing Ctrl+C handler")?;
    if enter && io::stdin().is_terminal() {
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

fn report(out: &Path, copy: bool) {
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    println!("{} ({:.1} MB)", out.display(), size as f64 / 1_048_576.0);
    if size > 10 * 1_048_576 {
        eprintln!("note: GitHub limits PR attachments to 10 MB; try --width 960, --fps 10 or --format mp4");
    }
    if copy {
        match clipboard::copy_file(out) {
            Ok(()) => eprintln!("copied to clipboard"),
            Err(e) => eprintln!("warning: couldn't copy to clipboard: {e:#}"),
        }
    }
}
