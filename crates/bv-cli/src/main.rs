mod clipboard;
mod control;
mod hotkey;
#[cfg(feature = "tray")]
mod settings;
mod tracker;
#[cfg(feature = "tray")]
mod tray;
mod ui;

use std::{
    io::{self, BufRead, IsTerminal},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bv_core::{
    export, ffmpeg, monitor, zoom::ZoomTrigger, Background, CaptureRegion, CursorTrack, Engine,
    ExportOptions, Exported, FrameStyle, MonitorChoice, OutputFormat, PointerStyle, RecordOptions,
    Recording, StylePreset, ZoomConfig,
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
    /// List visible windows (for --window).
    Windows,
    /// Stop recordings started by `bv record` in the background, wait for
    /// them to export, and print their results.
    Stop(StopArgs),
    /// Install or print the Claude skill for recording demos with bv.
    Skill(SkillArgs),
    /// Check that dependencies are installed.
    Doctor,
}

#[derive(Args)]
struct StopArgs {
    /// Give up waiting for the export after this many seconds.
    #[arg(long, default_value_t = 300, value_name = "SECS")]
    timeout: u64,
}

#[derive(Args)]
struct SkillArgs {
    #[command(subcommand)]
    action: SkillAction,
}

#[derive(Subcommand)]
enum SkillAction {
    /// Write the skill to ~/.claude/skills/better-video/SKILL.md (or --dir).
    Install {
        /// Skills folder to install into [default: ~/.claude/skills].
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Print the skill.
    Show,
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
    /// Before each recording, dim the screens and click one or drag out an
    /// area to record (Windows).
    #[arg(long, short = 's', conflicts_with_all = ["region", "monitor"])]
    select: bool,
    /// Record the front-most window whose title contains TITLE
    /// (case-insensitive; Windows). Its position when recording starts is
    /// used. See `bv windows`.
    #[arg(long, value_name = "TITLE", conflicts_with_all = ["region", "monitor", "select"])]
    window: Option<String>,
    /// Seconds of countdown after pressing Record in the --select bar.
    #[arg(long, default_value_t = 3, value_name = "SECS")]
    countdown: u32,
    /// Screen to capture with the ffmpeg engine (X11 display, avfoundation
    /// device or gdigrab target).
    #[arg(long)]
    display: Option<String>,
    /// Stop automatically after this many seconds.
    #[arg(long, short = 'd')]
    duration: Option<f64>,
    /// Capture frame rate [default: the export frame rate with the native
    /// engine, 30 with ffmpeg].
    #[arg(long)]
    capture_fps: Option<u32>,
    /// What captures the screen: `native` (built in, Windows only; GIFs
    /// need no ffmpeg) or `ffmpeg` [default: native on Windows].
    #[arg(long, value_enum)]
    engine: Option<EngineArg>,
    /// Keep the raw recording + cursor track next to the output so you can
    /// re-run `bv export` later (with different --cursor-* settings too).
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
    #[arg(long, value_name = "PATH")]
    track: Option<PathBuf>,
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
    /// Print the result as one line of JSON on stdout (path, bytes, format,
    /// width, height, seconds) instead of a sentence.
    #[arg(long)]
    json: bool,
    /// Draw a ring wherever you click.
    #[arg(long)]
    clicks: bool,
    /// Zoom magnification.
    #[arg(long, default_value_t = 2.0)]
    zoom_level: f64,
    /// What makes the camera zoom in: `click`, `hover` (where the cursor
    /// rests), `click-or-hover`, or `follow` (always zoomed, panning with
    /// the cursor). Implies zoom.
    #[arg(long, value_enum, value_name = "TRIGGER")]
    zoom_on: Option<ZoomOn>,
    /// Seconds to stay zoomed after a click or once the cursor moves on.
    #[arg(long, default_value_t = 1.5, value_name = "SECS")]
    zoom_hold: f64,
    /// Roughly how long the camera takes to settle, in seconds. Larger is
    /// smoother and slower.
    #[arg(long, default_value_t = 0.8, value_name = "SECS")]
    zoom_smooth: f64,
    /// For hover zoom: how long the cursor must stay put to count as
    /// resting, in seconds.
    #[arg(long, default_value_t = 0.6, value_name = "SECS")]
    zoom_dwell: f64,
    /// Seconds to cut from the start (e.g. reaching for the mouse).
    #[arg(long, default_value_t = 0.0, value_name = "SECS")]
    trim_start: f64,
    /// Seconds to cut from the end (e.g. pressing the stop key).
    #[arg(long, default_value_t = 0.0, value_name = "SECS")]
    trim_end: f64,
    /// Frame the recording: put it on a background with padding, rounded
    /// corners and a soft shadow. --width is then the whole frame's width.
    #[arg(long, value_enum, default_value_t = Style::None)]
    style: Style,
    /// Background colour behind the recording, #rrggbb, or two for a
    /// gradient (#rrggbb,#rrggbb). Implies --style clean if no style is set.
    #[arg(long, value_name = "COLOR[,COLOR]")]
    background: Option<Background>,
    /// Space around the recording in pixels [default: 8% of its width].
    /// Implies --style clean if no style is set.
    #[arg(long, value_name = "PX")]
    padding: Option<u32>,
    /// Corner radius of the recording in pixels [default: about 2% of its
    /// width]. Implies --style clean if no style is set.
    #[arg(long, value_name = "PX")]
    radius: Option<u32>,
    /// How the mouse pointer is shown: `drawn` (recorded without it, then
    /// drawn crisp, larger and smoothed at export; needs cursor tracking),
    /// `os` (the system pointer as captured) or `none` [default: drawn
    /// where cursor tracking works, else os; for `bv export`, what the
    /// recording was made with].
    #[arg(long, value_enum, value_name = "MODE")]
    cursor: Option<CursorMode>,
    /// Leave the mouse pointer out. Same as --cursor none.
    #[arg(long, conflicts_with = "cursor")]
    no_cursor: bool,
    /// Drawn pointer size, relative to a standard system arrow.
    #[arg(long, default_value_t = 1.5, value_name = "SCALE")]
    cursor_size: f64,
    /// Roughly how long the drawn pointer takes to catch up with the real
    /// one, in seconds; 0 follows the recorded path exactly. It's always
    /// exactly on target when you click.
    #[arg(long, default_value_t = 0.15, value_name = "SECS")]
    cursor_smooth: f64,
    /// Fade the drawn pointer out once it's been still this many seconds
    /// (it fades back in before it moves) [default: never].
    #[arg(long, value_name = "SECS")]
    cursor_hide: Option<f64>,
    /// Don't squash the drawn pointer while a button is held.
    #[arg(long)]
    no_cursor_bounce: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
enum CursorMode {
    /// Drawn at export from the cursor track.
    Drawn,
    /// The system pointer, captured in the frames.
    Os,
    /// No pointer.
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Style {
    /// The recording as is, edge to edge.
    None,
    /// Light grey background.
    Clean,
    /// Blue to violet gradient.
    Gradient,
    /// Dark slate background.
    Dark,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ZoomOn {
    Click,
    Hover,
    ClickOrHover,
    Follow,
}

impl From<ZoomOn> for ZoomTrigger {
    fn from(z: ZoomOn) -> Self {
        match z {
            ZoomOn::Click => ZoomTrigger::Click,
            ZoomOn::Hover => ZoomTrigger::Hover,
            ZoomOn::ClickOrHover => ZoomTrigger::ClickOrHover,
            ZoomOn::Follow => ZoomTrigger::Follow,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EngineArg {
    Native,
    Ffmpeg,
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

impl From<OutputFormat> for Format {
    fn from(f: OutputFormat) -> Self {
        match f {
            OutputFormat::Gif => Format::Gif,
            OutputFormat::Mp4 => Format::Mp4,
            OutputFormat::Webm => Format::Webm,
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

    /// `--zoom-on`, else `--zoom-follow`, else `-z` (click).
    fn trigger(&self) -> Option<ZoomTrigger> {
        self.zoom_on
            .map(Into::into)
            .or(self.zoom_follow.then_some(ZoomTrigger::Follow))
            .or(self.zoom.then_some(ZoomTrigger::Click))
    }

    fn zoom(&self) -> Option<ZoomConfig> {
        self.trigger().map(|trigger| ZoomConfig {
            level: self.zoom_level,
            trigger,
            hold_secs: self.zoom_hold.max(0.0),
            smoothing_secs: self.zoom_smooth.max(0.05),
            dwell_secs: self.zoom_dwell.max(0.1),
        })
    }

    /// Initial state of the --select options bar.
    fn choices(&self, format: OutputFormat) -> ui::Choices {
        ui::Choices {
            format,
            quality: ui::Quality::from_width(self.width),
            clicks: self.clicks,
            zoom: ui::ZoomMode::from_trigger(self.trigger()),
        }
    }

    /// Take on what was picked in the options bar.
    fn apply(&mut self, c: &ui::Choices) {
        self.format = Some(c.format.into());
        self.gif = false;
        self.width = c.quality.width();
        self.fps = Some(c.quality.fps());
        self.clicks = c.clicks;
        self.zoom = false;
        self.zoom_follow = false;
        self.zoom_on = c.zoom.trigger().map(|t| match t {
            ZoomTrigger::Click => ZoomOn::Click,
            ZoomTrigger::Hover => ZoomOn::Hover,
            ZoomTrigger::ClickOrHover => ZoomOn::ClickOrHover,
            ZoomTrigger::Follow => ZoomOn::Follow,
        });
    }

    fn fps(&self, format: OutputFormat) -> u32 {
        self.fps.unwrap_or(ExportOptions::new("", "", format).fps)
    }

    fn needs_cursor(&self) -> bool {
        self.zoom().is_some() || self.clicks
    }

    /// `--cursor`, or `none` for `--no-cursor`; `None` picks a default.
    fn cursor_mode(&self) -> Option<CursorMode> {
        if self.no_cursor {
            Some(CursorMode::None)
        } else {
            self.cursor
        }
    }

    fn pointer_style(&self) -> PointerStyle {
        PointerStyle {
            scale: self.cursor_size.clamp(0.25, 8.0),
            smoothing_secs: self.cursor_smooth.max(0.0),
            bounce: !self.no_cursor_bounce,
            hide_after: self.cursor_hide.map(|s| s.max(0.0)),
        }
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
        opts.style = self.style();
        opts
    }

    /// `--style` plus any finer --background / --padding / --radius.
    fn style(&self) -> Option<FrameStyle> {
        let preset = match self.style {
            Style::Clean => StylePreset::Clean,
            Style::Gradient => StylePreset::Gradient,
            Style::Dark => StylePreset::Dark,
            Style::None => {
                let tweaked =
                    self.background.is_some() || self.padding.is_some() || self.radius.is_some();
                if !tweaked {
                    return None;
                }
                StylePreset::Clean
            }
        };
        let mut style = preset.style();
        if let Some(bg) = self.background {
            style.background = bg;
        }
        style.padding = self.padding.or(style.padding);
        style.radius = self.radius.or(style.radius);
        Some(style)
    }
}

/// Exit codes: 0 success, 1 error, 2 bad arguments (from clap), 3 the
/// user cancelled the area picker or discarded the take.
fn main() -> ExitCode {
    // Physical pixels everywhere, so monitor bounds, capture offsets and
    // cursor positions agree on scaled displays.
    monitor::set_dpi_aware();
    let result = match Cli::parse().command {
        Command::Record(args) => record(args),
        Command::Hotkey(args) => hotkey_mode(args),
        #[cfg(feature = "tray")]
        Command::Tray(args) => tray::run(args),
        Command::Export(args) => export_cmd(args),
        Command::Monitors => list_monitors(),
        Command::Windows => list_windows(),
        Command::Stop(args) => stop(args),
        Command::Skill(args) => skill(args),
        Command::Doctor => doctor(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.is::<ui::Cancelled>() => {
            eprintln!("Cancelled.");
            ExitCode::from(3)
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn list_windows() -> Result<()> {
    let list = bv_core::window::windows();
    if list.is_empty() {
        println!("Window listing isn't supported on this platform yet; use --region.");
    }
    for w in list {
        println!("{w}");
    }
    Ok(())
}

fn stop(args: StopArgs) -> Result<()> {
    let results = control::stop_all(Duration::from_secs(args.timeout))?;
    if results.is_empty() {
        eprintln!("No recordings running.");
    }
    for r in results {
        println!("{r}");
    }
    Ok(())
}

/// The skill, bundled so `bv skill install` works from the binary alone.
const SKILL: &str = include_str!("../../../skills/better-video/SKILL.md");

fn skill(args: SkillArgs) -> Result<()> {
    match args.action {
        SkillAction::Show => print!("{SKILL}"),
        SkillAction::Install { dir } => {
            let dir = match dir {
                Some(d) => d,
                None => {
                    let home = std::env::var_os("USERPROFILE")
                        .or_else(|| std::env::var_os("HOME"))
                        .context("can't find your home folder; pass --dir")?;
                    PathBuf::from(home).join(".claude").join("skills")
                }
            };
            let path = dir.join("better-video").join("SKILL.md");
            std::fs::create_dir_all(path.parent().unwrap_or(&dir))?;
            std::fs::write(&path, SKILL).with_context(|| format!("writing {}", path.display()))?;
            println!("Installed {}", path.display());
        }
    }
    Ok(())
}

impl CaptureArgs {
    /// The region to record: `--region`, else `--monitor`, else (on Windows,
    /// where the desktop spans every screen) the primary monitor.
    fn capture_region(&self) -> Result<Option<CaptureRegion>> {
        if let Some(title) = &self.window {
            let w = bv_core::window::find(title).map_err(anyhow::Error::msg)?;
            eprintln!("Recording window {:?} at {}", w.title, w.region);
            return Ok(Some(w.region));
        }
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

    fn engine(&self) -> Engine {
        match self.engine {
            Some(EngineArg::Native) => Engine::Native,
            Some(EngineArg::Ffmpeg) => Engine::Ffmpeg,
            // --display names an ffmpeg device.
            None if self.display.is_some() => Engine::Ffmpeg,
            None => Engine::default_for_platform(),
        }
    }

    /// Native capture only needs as many frames as the export keeps;
    /// ffmpeg's intermediate may be re-exported at any rate.
    fn capture_fps(&self, engine: Engine, format: OutputFormat) -> u32 {
        self.capture_fps.unwrap_or(match engine {
            Engine::Native => self.export.fps(format),
            Engine::Ffmpeg => 30,
        })
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
        .unwrap_or_else(|| fresh_output(Path::new(""), format));
    let stop = stop_signal(true)?;
    // A cancelled picker returns Cancelled, which exits with code 3.
    let mut session = Session::start(&args.capture, output.clone(), format)?;
    // So `bv stop` (from a script or agent) can end it.
    let active = control::Active::register(&output)?;
    eprintln!("● Recording… press Enter or Ctrl+C to stop (or run `bv stop`)");
    while !stop.load(Ordering::Relaxed) && !session.finished() && !active.stop_requested() {
        thread::sleep(Duration::from_millis(50));
    }
    // Discarding from the recording pill also returns Cancelled.
    let exported = session.finish()?;
    active.done(&result_json(&exported));
    Ok(())
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
                let output = fresh_output(&dir, format);
                match Session::start(&args.capture, output, format) {
                    Ok(s) => {
                        eprintln!("● Recording… press {} to stop", args.key);
                        session = Some(s);
                    }
                    Err(e) if e.is::<ui::Cancelled>() => {}
                    Err(e) => eprintln!("error: {e:#}"),
                }
            }
            Some(_) if pressed || quitting || ended => {
                if let Some(s) = session.take() {
                    match s.finish() {
                        // Discarded from the recording pill.
                        Err(e) if e.is::<ui::Cancelled>() => {}
                        Err(e) => eprintln!("error: {e:#}"),
                        Ok(_) => {}
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
    indicator: Option<ui::Indicator>,
    /// What was chosen in the area picker, if it was shown.
    picked: Option<ui::Picked>,
    /// Settings for this clip: the caller's, plus anything picked in the
    /// options bar.
    args: CaptureArgs,
    output: PathBuf,
    format: OutputFormat,
    /// How the pointer ends up in the video.
    cursor: CursorMode,
}

impl Session {
    fn start(args: &CaptureArgs, mut output: PathBuf, mut format: OutputFormat) -> Result<Self> {
        let mut args = args.clone();
        let picked = if args.select {
            let req = ui::PickRequest {
                choices: args.export.choices(format),
                save_dir: absolute(&output)
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default(),
                countdown: args.countdown,
            };
            let picked = ui::pick(req)?.ok_or(ui::Cancelled)?;
            args.export.apply(&picked.choices);
            if picked.choices.format != format {
                format = picked.choices.format;
                output.set_extension(format.extension());
            }
            Some(picked)
        } else {
            None
        };
        let region = match picked {
            Some(p) => Some(p.region),
            None => args.capture_region()?,
        };

        // Drawing the pointer at export needs to know where it was.
        let tracking = CursorTracker::available();
        let cursor = match args.export.cursor_mode() {
            Some(CursorMode::Drawn) if !tracking => {
                eprintln!("warning: cursor tracking unavailable; recording the system pointer");
                CursorMode::Os
            }
            Some(mode) => mode,
            None if tracking => CursorMode::Drawn,
            None => CursorMode::Os,
        };

        let engine = args.engine();
        let raw_ext = format!("raw.{}", engine.raw_extension());
        let raw = if args.keep_raw {
            output.with_extension(&raw_ext)
        } else {
            let dir = std::env::temp_dir().join("better-video");
            std::fs::create_dir_all(&dir)?;
            clean_stale(&dir);
            dir.join(
                output
                    .with_extension(&raw_ext)
                    .file_name()
                    .context("bad output path")?,
            )
        };
        let opts = RecordOptions {
            fps: args.capture_fps(engine, format),
            region,
            display: args.display.clone(),
            max_duration: args.duration.map(Duration::from_secs_f64),
            engine,
            draw_cursor: cursor == CursorMode::Os,
            ..RecordOptions::new(&raw)
        };

        let recording = Recording::start(&opts)?;
        let tracker = if args.export.needs_cursor() || cursor == CursorMode::Drawn {
            let t = CursorTracker::start(recording.started(), region);
            if t.is_none() {
                eprintln!(
                    "warning: cursor tracking unavailable; exporting without zoom/clicks/pointer"
                );
            }
            t
        } else {
            None
        };
        let indicator = ui::Indicator::show(region.unwrap_or_else(monitor::virtual_screen));
        Ok(Self {
            recording,
            tracker,
            indicator,
            picked,
            args,
            output,
            format,
            cursor,
        })
    }

    /// True once the duration limit passed, capture stopped by itself, or
    /// Stop or Discard was clicked on the pill.
    fn finished(&mut self) -> bool {
        self.recording.finished()
            || self
                .indicator
                .as_ref()
                .and_then(|i| i.requested())
                .is_some()
    }

    /// What was chosen in the area picker, if it was shown.
    #[cfg_attr(not(feature = "tray"), allow(dead_code))]
    fn picked(&self) -> Option<ui::Picked> {
        self.picked
    }

    /// Stop recording, export, and report the result.
    fn finish(mut self) -> Result<Exported> {
        let discard =
            self.indicator.as_ref().and_then(|i| i.requested()) == Some(ui::StopRequest::Discard);
        // Close the on-screen frame right away.
        self.indicator = None;
        let args = &self.args;
        let secs = self.recording.elapsed().as_secs_f64();
        let mut track = self.tracker.map(CursorTracker::finish);
        let drawn = self.cursor == CursorMode::Drawn;
        if let Some(t) = &mut track {
            // So `bv export` knows to draw it again.
            t.draw_pointer = drawn;
        }
        let recorded = self.recording.stop()?;
        let recorded_stats = recorded.stats;
        let raw = recorded.path;
        if let Some(t) = &mut track {
            t.shift_earlier(recorded.first_frame_delay);
        }
        if discard {
            let _ = std::fs::remove_file(&raw);
            eprintln!("■ Discarded");
            return Err(ui::Cancelled.into());
        }
        match &recorded_stats {
            Some(s) => eprintln!(
                "■ Stopped after {secs:.1}s ({} frames, {} changed, {}), exporting…",
                s.captured, s.stored, s.backend
            ),
            None => eprintln!("■ Stopped after {secs:.1}s, exporting…"),
        }
        if let (Some(s), true) = (&recorded_stats, std::env::var_os("BV_PROFILE").is_some()) {
            let per = |secs: f64, n: u64| secs * 1000.0 / n.max(1) as f64;
            eprintln!(
                "  capture: {:.2} ms/grab, {:.2} ms/stored frame",
                per(s.grab_secs, s.captured),
                per(s.store_secs, s.stored)
            );
        }

        if args.keep_raw {
            if let Some(t) = &track {
                t.save(&cursor_sidecar(&raw))?;
            }
        }

        let mut eopts = args.export.options(raw.clone(), self.output, self.format);
        eopts.pointer = drawn.then(|| args.export.pointer_style());
        eopts.cursor = track;
        let export_started = Instant::now();
        let result = export(&eopts);
        if std::env::var_os("BV_PROFILE").is_some() {
            eprintln!("  export: {:.2} s", export_started.elapsed().as_secs_f64());
        }
        if !args.keep_raw {
            let _ = std::fs::remove_file(&raw);
        }
        let out = result?;
        report(&out, args.export.copy, args.export.json);
        Ok(out)
    }
}

/// Delete leftover recordings in the temp folder from runs that crashed or
/// were killed before they could clean up. Only files older than a day, so
/// a recording in progress in another bv is never touched.
fn clean_stale(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let day = Duration::from_secs(24 * 60 * 60);
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > day));
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `path` made absolute against the current directory.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn export_cmd(args: ExportCmd) -> Result<()> {
    let format = args.export.format(args.output.as_deref());
    let output = args.output.clone().unwrap_or_else(|| {
        let stem = args.input.with_extension("");
        // "foo.raw.mkv" -> "foo.gif"
        stem.with_extension("").with_extension(format.extension())
    });
    let mut opts = args.export.options(args.input.clone(), output, format);
    let mode = args.export.cursor_mode();
    let path = args
        .track
        .clone()
        .unwrap_or_else(|| cursor_sidecar(&args.input));
    let required = args.export.needs_cursor() || mode == Some(CursorMode::Drawn);
    if required || args.track.is_some() || path.exists() {
        opts.cursor = Some(CursorTrack::load(&path).with_context(|| {
            "zoom/clicks/--cursor drawn need a cursor track; pass --track or record with --keep-raw"
        })?);
    }
    // The recording has the system pointer in it unless it was made to
    // have one drawn (or none).
    let recorded_drawn = opts.cursor.as_ref().is_some_and(|t| t.draw_pointer);
    let draw = match mode {
        None => recorded_drawn,
        Some(CursorMode::Drawn) => {
            if !recorded_drawn {
                eprintln!("note: if this recording shows the system pointer, it will show twice");
            }
            true
        }
        Some(CursorMode::Os) => {
            if recorded_drawn {
                eprintln!("note: this recording has no system pointer; --cursor os shows none");
            }
            false
        }
        Some(CursorMode::None) => false,
    };
    opts.pointer = draw.then(|| args.export.pointer_style());
    report(&export(&opts)?, args.export.copy, args.export.json);
    Ok(())
}

fn doctor() -> Result<()> {
    let native = Engine::default_for_platform() == Engine::Native;
    let mut mp4_native = false;
    if native {
        let primary = monitor::monitors()
            .into_iter()
            .find(|m| m.primary)
            .map(|m| m.region);
        // A small corner of the primary screen is enough to probe.
        let area = primary.map(|r| CaptureRegion {
            width: r.width.min(64),
            height: r.height.min(64),
            ..r
        });
        match area.map(bv_core::record::probe_capture) {
            Some(Ok(backend)) => println!("✓ screen capture: {backend} (built in)"),
            Some(Err(e)) => println!("✗ screen capture failed: {e:#}"),
            None => println!("✗ no monitors found"),
        }
        println!("✓ GIF encoder: built in");
        mp4_native = bv_core::record::builtin_mp4();
        if mp4_native {
            println!("✓ MP4 encoder: built in (Windows Media Foundation)");
        } else {
            println!("- MP4 encoder: Media Foundation unavailable (Windows N without the Media Feature Pack?); MP4 needs ffmpeg");
        }
    }
    match ffmpeg::version() {
        Some(v) => println!("✓ {v}"),
        None if native && mp4_native => println!("- ffmpeg not found: only needed for WebM"),
        None if native => println!("- ffmpeg not found: only needed for MP4/WebM"),
        None => println!("✗ ffmpeg not found: install it or set BV_FFMPEG"),
    }
    let cursor = CursorTracker::start(Instant::now(), None).map(CursorTracker::finish);
    match cursor {
        Some(_) => println!("✓ cursor tracking available (zoom, clicks, drawn pointer)"),
        None => println!("✗ cursor tracking unavailable (no zoom/clicks; system pointer)"),
    }
    println!("  platform: {:?}", ffmpeg::Platform::current());
    println!("  network: none (bv never connects anywhere; everything stays on this machine)");
    Ok(())
}

/// Flag set by Ctrl+C, or by pressing Enter (when `enter` and attached to
/// a terminal).
fn stop_signal(enter: bool) -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    ctrlc::set_handler(move || s.store(true, Ordering::Relaxed))
        .context("installing Ctrl+C handler")?;
    if enter && io::stdin().is_terminal() && in_foreground() {
        let s = stop.clone();
        thread::spawn(move || {
            let _ = io::stdin().lock().lines().next();
            s.store(true, Ordering::Relaxed);
        });
    }
    Ok(stop)
}

/// Whether we own the terminal. Reading it from the background (`bv record
/// ... &` in an interactive shell) gets the whole process stopped by the
/// kernel, so then only `bv stop` (or a signal) ends the recording.
#[cfg(unix)]
fn in_foreground() -> bool {
    unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) == libc::getpgrp() }
}

#[cfg(not(unix))]
fn in_foreground() -> bool {
    true
}

/// A new file in `dir` named after the current time, with `-2`, `-3`, …
/// added if a recording from the same second already exists.
fn fresh_output(dir: &Path, format: OutputFormat) -> PathBuf {
    let first = dir.join(default_output(format));
    let stem = first.with_extension("");
    let numbered = |n: u32| {
        let mut name = stem.clone().into_os_string();
        name.push(format!("-{n}.{}", format.extension()));
        PathBuf::from(name)
    };
    std::iter::once(first.clone())
        .chain((2..).map(numbered))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// `bv-2026-10-11_15-48-12.gif`: local time on Windows, UTC elsewhere.
fn default_output(format: OutputFormat) -> PathBuf {
    let (y, mo, d, h, mi, s) = now();
    PathBuf::from(format!(
        "bv-{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}.{}",
        format.extension()
    ))
}

#[cfg(windows)]
fn now() -> (u32, u32, u32, u32, u32, u32) {
    let mut t = unsafe { std::mem::zeroed() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    let t: windows_sys::Win32::Foundation::SYSTEMTIME = t;
    let f = |v: u16| v as u32;
    (
        f(t.wYear),
        f(t.wMonth),
        f(t.wDay),
        f(t.wHour),
        f(t.wMinute),
        f(t.wSecond),
    )
}

#[cfg(not(windows))]
fn now() -> (u32, u32, u32, u32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    (
        y as u32,
        m,
        d,
        (rem / 3600) as u32,
        (rem / 60 % 60) as u32,
        (rem % 60) as u32,
    )
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
#[cfg(not(windows))]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

/// "412 KB", "3.4 MB".
fn human_size(bytes: u64) -> String {
    if bytes < 1_048_576 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    }
}

fn cursor_sidecar(raw: &Path) -> PathBuf {
    raw.with_extension("cursor.json")
}

/// The result as JSON, for `--json` and `bv stop`.
fn result_json(out: &Exported) -> serde_json::Value {
    let bytes = std::fs::metadata(&out.path).map_or(0, |m| m.len());
    let format = OutputFormat::from_path(&out.path).map_or("unknown", |f| f.extension());
    serde_json::json!({
        "path": absolute(&out.path),
        "bytes": bytes,
        "format": format,
        "width": out.width,
        "height": out.height,
        "seconds": (out.seconds * 100.0).round() / 100.0,
    })
}

/// Print the result: a sentence, or one JSON line with `--json`. Progress
/// and notes go to stderr, so stdout is only the result either way.
fn report(out: &Exported, copy: bool, json: bool) {
    let size = std::fs::metadata(&out.path).map_or(0, |m| m.len());
    if json {
        println!("{}", result_json(out));
    } else {
        println!(
            "Saved {} ({}, {}×{}, {:.1}s)",
            absolute(&out.path).display(),
            human_size(size),
            out.width,
            out.height,
            out.seconds
        );
    }
    if size > 10 * 1_048_576 {
        eprintln!("note: GitHub limits PR attachments to 10 MB; try --width 960, --fps 10 or --format mp4");
    }
    if copy {
        match clipboard::copy_file(&out.path) {
            Ok(()) => eprintln!("copied to clipboard"),
            Err(e) => eprintln!("warning: couldn't copy to clipboard: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(1000), "1 KB");
        assert_eq!(human_size(421_000), "412 KB");
        assert_eq!(human_size(3_565_158), "3.4 MB");
    }

    #[test]
    fn fresh_output_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("bv-fresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = fresh_output(&dir, OutputFormat::Gif);
        std::fs::write(&a, b"x").unwrap();
        let b = fresh_output(&dir, OutputFormat::Gif);
        assert_ne!(a, b);
        assert!(!b.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn output_names_sort_by_time() {
        let name = default_output(OutputFormat::Gif);
        let name = name.to_string_lossy();
        assert!(
            name.starts_with("bv-20") && name.ends_with(".gif"),
            "{name}"
        );
        assert_eq!(name.len(), "bv-2026-10-11_15-48-12.gif".len());
    }

    /// Every `--flag` the skill and the README mention must exist, so the
    /// docs can't drift from the CLI (e.g. after a flag is renamed).
    #[test]
    fn documented_flags_exist() {
        use clap::CommandFactory;
        fn collect(cmd: &clap::Command, flags: &mut std::collections::HashSet<String>) {
            flags.extend(
                cmd.get_arguments()
                    .filter_map(|a| a.get_long())
                    .map(String::from),
            );
            for sub in cmd.get_subcommands() {
                collect(sub, flags);
            }
        }
        let mut flags =
            std::collections::HashSet::from(["help".to_string(), "version".to_string()]);
        collect(&Cli::command(), &mut flags);
        // Flags of other tools the docs show (cargo, gh, git, xdotool, Chromium).
        const OTHER_TOOLS: &[&str] = &[
            "path",
            "features",
            "attach",
            "body-file",
            "title",
            "detach",
            "orphan",
            "shell",
            "name",
            "delay",
            "app",
            "window-size",
            "window-position",
            "user-data-dir",
            "no-first-run",
        ];
        for (doc, text) in [
            ("SKILL.md", SKILL),
            ("README.md", include_str!("../../../README.md")),
        ] {
            for (i, _) in text.match_indices("--") {
                let rest = &text[i + 2..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
                    .unwrap_or(rest.len());
                let flag = rest[..end].trim_end_matches('-');
                if flag.is_empty() || text[..i].ends_with('-') {
                    continue;
                }
                assert!(
                    flags.contains(flag) || OTHER_TOOLS.contains(&flag),
                    "{doc} mentions --{flag}, which bv doesn't have (add other tools' flags to OTHER_TOOLS)"
                );
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_737), (2026, 10, 11));
    }
}
