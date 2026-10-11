//! Menu bar (macOS) / system tray (Windows, Linux) app.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::Ordering,
    thread,
    time::Instant,
};

use anyhow::{Context, Result};
use tao::{
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoopBuilder},
};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::{
    default_output,
    hotkey::{self, EdgeDetector, Hotkey},
    stop_signal, CaptureArgs, Format, HotkeyArgs, Session,
};

enum UserEvent {
    Menu(MenuEvent),
    Exported(Result<PathBuf, String>),
}

enum State {
    Idle,
    Recording(Box<Session>),
    Exporting,
}

struct Items {
    record: MenuItem,
    zoom: CheckMenuItem,
    clicks: CheckMenuItem,
    copy: CheckMenuItem,
    formats: Vec<(CheckMenuItem, Format)>,
    last: MenuItem,
    open_dir: MenuItem,
    quit: MenuItem,
    key_label: String,
}

impl Items {
    fn new(capture: &CaptureArgs, key_label: &str) -> Self {
        let e = &capture.export;
        let current = e.format(None);
        let formats = [
            (Format::Gif, "GIF"),
            (Format::Mp4, "MP4"),
            (Format::Webm, "WebM"),
        ]
        .into_iter()
        .map(|(f, label)| {
            let checked = bv_core::OutputFormat::from(f) == current;
            (CheckMenuItem::new(label, true, checked, None), f)
        })
        .collect();
        let items = Self {
            record: MenuItem::new("", true, None),
            zoom: CheckMenuItem::new("Zoom on clicks", true, e.zoom || e.zoom_follow, None),
            clicks: CheckMenuItem::new("Highlight clicks", true, e.clicks, None),
            copy: CheckMenuItem::new("Copy to clipboard", true, e.copy, None),
            formats,
            last: MenuItem::new("Open last recording", false, None),
            open_dir: MenuItem::new("Open recordings folder", true, None),
            quit: MenuItem::new("Quit", true, None),
            key_label: key_label.to_owned(),
        };
        items.show_idle();
        items
    }

    fn menu(&self) -> Result<Menu> {
        let format = Submenu::new("Format", true);
        for (item, _) in &self.formats {
            format.append(item)?;
        }
        Ok(Menu::with_items(&[
            &self.record,
            &PredefinedMenuItem::separator(),
            &self.zoom,
            &self.clicks,
            &self.copy,
            &format,
            &PredefinedMenuItem::separator(),
            &self.last,
            &self.open_dir,
            &PredefinedMenuItem::separator(),
            &self.quit,
        ])?)
    }

    fn show_idle(&self) {
        self.record
            .set_text(format!("Start recording ({})", self.key_label));
        self.record.set_enabled(true);
    }

    fn show_recording(&self) {
        self.record
            .set_text(format!("Stop recording ({})", self.key_label));
    }

    fn show_exporting(&self) {
        self.record.set_text("Exporting…");
        self.record.set_enabled(false);
    }

    fn select_format(&self, format: Format) {
        for (item, f) in &self.formats {
            item.set_checked(*f == format);
        }
    }

    /// Copy the menu's current settings into `capture`.
    fn apply(&self, capture: &mut CaptureArgs) {
        let e = &mut capture.export;
        if !self.zoom.is_checked() {
            e.zoom = false;
            e.zoom_follow = false;
        } else if !e.zoom_follow {
            e.zoom = true;
        }
        e.clicks = self.clicks.is_checked();
        e.copy = self.copy.is_checked();
        if let Some((_, f)) = self.formats.iter().find(|(i, _)| i.is_checked()) {
            e.format = Some(*f);
            e.gif = false;
        }
    }
}

pub fn run(args: HotkeyArgs) -> Result<()> {
    let key: Hotkey = args.key.parse().map_err(anyhow::Error::msg)?;
    let mut keys = hotkey::keyboard();
    if keys.is_none() {
        eprintln!("warning: keyboard access unavailable; hotkey disabled (menu still works)");
    }
    let dir = args.dir.clone().unwrap_or_else(default_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut capture = args.capture.clone();
    capture.export.copy |= !args.no_copy;
    let quit = stop_signal(false)?;

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(UserEvent::Menu(e));
    }));

    let items = Items::new(&capture, &args.key);
    let menu = items.menu()?;
    let mut tray: Option<TrayIcon> = None;
    let mut state = State::Idle;
    let mut edge = EdgeDetector::default();
    let mut last: Option<PathBuf> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + hotkey::POLL_INTERVAL);
        let mut toggle = false;

        match event {
            // macOS requires the tray to be created once the loop is running.
            Event::NewEvents(StartCause::Init) => {
                match TrayIconBuilder::new()
                    .with_menu(Box::new(menu.clone()))
                    .with_tooltip("better-video")
                    .with_icon(icon(false))
                    .build()
                {
                    Ok(t) => {
                        set_icon(&t, false);
                        tray = Some(t);
                        eprintln!("better-video is in your menu bar / tray");
                    }
                    Err(e) => {
                        eprintln!("error: couldn't create tray icon: {e}");
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                let id = e.id();
                if id == items.record.id() {
                    toggle = true;
                } else if id == items.quit.id() {
                    quit.store(true, Ordering::Relaxed);
                } else if id == items.open_dir.id() {
                    open_path(&dir);
                } else if id == items.last.id() {
                    if let Some(p) = &last {
                        open_path(p);
                    }
                } else if let Some((_, f)) = items.formats.iter().find(|(i, _)| i.id() == id) {
                    // Behave like radio buttons.
                    items.select_format(*f);
                }
            }
            Event::UserEvent(UserEvent::Exported(result)) => {
                state = State::Idle;
                items.show_idle();
                match result {
                    Ok(path) => {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        items.last.set_text(format!("Open {name}"));
                        items.last.set_enabled(true);
                        if let Some(t) = &tray {
                            let _ = t.set_tooltip(Some(format!("better-video — saved {name}")));
                        }
                        last = Some(path);
                    }
                    Err(e) => eprintln!("error: {e}"),
                }
            }
            _ => {}
        }

        if let Some(k) = keys.as_mut() {
            toggle |= edge.pressed(key.held(&k()));
        }
        if let State::Recording(s) = &mut state {
            toggle |= s.finished();
        }
        let quitting = quit.load(Ordering::Relaxed);

        if toggle || quitting {
            state = match std::mem::replace(&mut state, State::Exporting) {
                State::Idle if quitting => State::Idle,
                State::Idle => {
                    items.apply(&mut capture);
                    let format = capture.export.format(None);
                    match Session::start(&capture, dir.join(default_output(format)), format) {
                        Ok(s) => {
                            items.show_recording();
                            tray.iter().for_each(|t| set_icon(t, true));
                            State::Recording(Box::new(s))
                        }
                        Err(e) => {
                            if !e.is::<crate::ui::Cancelled>() {
                                eprintln!("error: {e:#}");
                            }
                            State::Idle
                        }
                    }
                }
                State::Recording(session) => {
                    items.apply(&mut capture);
                    items.show_exporting();
                    tray.iter().for_each(|t| set_icon(t, false));
                    let capture = capture.clone();
                    let proxy = proxy.clone();
                    thread::spawn(move || {
                        let result = (*session).finish(&capture).map_err(|e| format!("{e:#}"));
                        let _ = proxy.send_event(UserEvent::Exported(result));
                    });
                    State::Exporting
                }
                // Wait for the export; quitting resumes once it lands.
                State::Exporting => State::Exporting,
            };
        }

        if quitting && matches!(state, State::Idle) {
            tray.take();
            *control_flow = ControlFlow::Exit;
        }
    })
}

/// `~/Movies/better-video` on macOS, `~/Videos/better-video` elsewhere.
fn default_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let videos = if cfg!(target_os = "macos") {
        "Movies"
    } else {
        "Videos"
    };
    home.join(videos).join("better-video")
}

fn open_path(path: &Path) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    if let Err(e) = Command::new(program).arg(path).spawn() {
        eprintln!("error: couldn't run {program}: {e}");
    }
}

fn set_icon(tray: &TrayIcon, recording: bool) {
    // The idle icon is a template on macOS so it follows the menu bar's
    // light/dark appearance; the recording icon stays red.
    #[cfg(target_os = "macos")]
    let result = if recording {
        tray.set_icon(Some(icon(true)))
    } else {
        tray.set_icon_templated(Some(icon(false)))
    };
    #[cfg(not(target_os = "macos"))]
    let result = tray.set_icon(Some(icon(recording)));
    if let Err(e) = result {
        eprintln!("warning: couldn't update tray icon: {e}");
    }
}

/// 32×32 "record" glyph: a ring with a dot (idle) or a solid red disc
/// (recording).
fn icon(recording: bool) -> Icon {
    const N: u32 = 32;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    let c = N as f64 / 2.0;
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f64 + 0.5 - c).powi(2) + (y as f64 + 0.5 - c).powi(2)).sqrt();
            // Anti-aliased coverage of a disc of radius r.
            let disc = |r: f64| (r + 0.5 - d).clamp(0.0, 1.0);
            let (rgb, a) = if recording {
                ([230, 40, 40], disc(13.0))
            } else {
                let ring = disc(14.0) - disc(11.0);
                ([40, 40, 40], (ring + disc(7.0)).min(1.0))
            };
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (a * 255.0) as u8]);
        }
    }
    Icon::from_rgba(rgba, N, N).expect("valid icon dimensions")
}
