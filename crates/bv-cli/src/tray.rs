//! Menu bar (macOS) / system tray (Windows, Linux) app.
//!
//! Click the icon or press the hotkey to record. On Windows that opens the
//! area picker, whose options bar holds the format/size/click settings;
//! elsewhere it records the primary screen with the remembered settings.
//! The menu (right-click on Windows) shows where recordings are saved and
//! opens the last one.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::Ordering,
    thread,
    time::Instant,
};

use anyhow::{Context, Result};
use bv_core::OutputFormat;
use tao::{
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoopBuilder},
};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

use crate::{
    fresh_output,
    hotkey::{self, EdgeDetector, Hotkey},
    human_size,
    settings::Settings,
    stop_signal,
    ui::{self, Choices, Quality},
    HotkeyArgs, Session,
};

enum UserEvent {
    Menu(MenuEvent),
    Tray(TrayIconEvent),
    /// A session started; `Err(None)` means the picker was cancelled.
    Started(Result<Box<Session>, Option<String>>),
    Exported(Result<PathBuf, String>),
    Folder(Option<PathBuf>),
}

enum State {
    Idle,
    /// Picker open, or capture starting.
    Starting,
    Recording(Box<Session>),
    Exporting,
}

struct Items {
    record: MenuItem,
    again: MenuItem,
    last: MenuItem,
    reveal: MenuItem,
    saving_to: MenuItem,
    open_dir: MenuItem,
    change_dir: MenuItem,
    copy: CheckMenuItem,
    quit: MenuItem,
    key_label: String,
}

impl Items {
    fn new(key_label: &str, dir: &Path, copy: bool, has_area: bool) -> Self {
        let items = Self {
            record: MenuItem::new("", true, None),
            again: MenuItem::new("Record same area again", has_area, None),
            last: MenuItem::new("Open last recording", false, None),
            reveal: MenuItem::new("Show last recording in folder", false, None),
            saving_to: MenuItem::new("", false, None),
            open_dir: MenuItem::new("Open recordings folder", true, None),
            change_dir: MenuItem::new("Change folder…", true, None),
            copy: CheckMenuItem::new("Copy each recording to the clipboard", true, copy, None),
            quit: MenuItem::new("Quit better-video", true, None),
            key_label: key_label.to_owned(),
        };
        items.show_dir(dir);
        items.show_idle(has_area);
        items
    }

    fn menu(&self) -> Result<Menu> {
        let sep = PredefinedMenuItem::separator;
        let menu = Menu::with_items(&[
            &self.record,
            &self.again,
            &sep(),
            &self.last,
            &self.reveal,
            &sep(),
            &self.saving_to,
            &self.open_dir,
        ])?;
        if cfg!(windows) {
            menu.append(&self.change_dir)?;
        }
        menu.append_items(&[&sep(), &self.copy, &sep(), &self.quit])?;
        Ok(menu)
    }

    /// The save folder is always visible in the menu.
    fn show_dir(&self, dir: &Path) {
        self.saving_to
            .set_text(format!("Saving to {}", dir.display()));
    }

    /// Ready to record; "same area again" only when there is one.
    fn show_idle(&self, can_repeat: bool) {
        self.again.set_enabled(can_repeat);
        let label = if cfg!(windows) {
            "New recording…"
        } else {
            "Start recording"
        };
        self.record
            .set_text(format!("{label}  ({})", self.key_label));
        self.record.set_enabled(true);
    }

    fn show_busy(&self, text: &str) {
        self.record.set_text(text);
        self.record.set_enabled(false);
        self.again.set_enabled(false);
    }

    fn show_recording(&self) {
        self.record
            .set_text(format!("Stop recording  ({})", self.key_label));
        self.record.set_enabled(true);
    }

    fn show_last(&self, path: &Path) {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let size = std::fs::metadata(path).map_or(0, |m| m.len());
        self.last
            .set_text(format!("Open {name}  ({})", human_size(size)));
        self.last.set_enabled(true);
        self.reveal.set_enabled(true);
    }
}

pub fn run(args: HotkeyArgs) -> Result<()> {
    let key: Hotkey = args.key.parse().map_err(anyhow::Error::msg)?;
    let mut keys = hotkey::keyboard();
    if keys.is_none() {
        eprintln!("warning: keyboard access unavailable; hotkey disabled (menu still works)");
    }
    let mut settings = Settings::load();
    // Forget an area whose monitor has since been unplugged or moved.
    let monitors = bv_core::monitor::monitors();
    if !monitors.is_empty() {
        settings.last_area = settings
            .last_area
            .filter(|&a| bv_core::monitor::is_on_screen(a, &monitors));
    }
    let mut dir = args
        .dir
        .clone()
        .or_else(|| settings.dir.clone())
        .unwrap_or_else(default_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let base = args.capture.clone();
    // Small GIFs unless told otherwise: that's what this is for.
    let mut choices = settings.choices.unwrap_or(Choices {
        format: OutputFormat::Gif,
        quality: Quality::Small,
        clicks: base.export.clicks,
        zoom: base.export.zoom || base.export.zoom_follow,
    });
    let copy = !args.no_copy && settings.copy.unwrap_or(true);
    let quit = stop_signal(false)?;

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(UserEvent::Menu(e));
    }));
    let tray_proxy = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |e| {
        let _ = tray_proxy.send_event(UserEvent::Tray(e));
    }));

    let items = Items::new(&args.key, &dir, copy, settings.last_area.is_some());
    let menu = items.menu()?;
    let mut tray: Option<TrayIcon> = None;
    let mut state = State::Idle;
    let mut edge = EdgeDetector::default();
    let mut last: Option<PathBuf> = None;
    let launched = Instant::now();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + hotkey::POLL_INTERVAL);
        let mut toggle = false;
        let mut again = false;

        match event {
            // macOS requires the tray to be created once the loop is running.
            Event::NewEvents(StartCause::Init) => {
                match TrayIconBuilder::new()
                    .with_menu(Box::new(menu.clone()))
                    // On Windows left-click records and the menu is on
                    // right-click.
                    .with_menu_on_left_click(!cfg!(windows))
                    .with_tooltip(format!(
                        "better-video — click or press {} to record",
                        args.key
                    ))
                    .with_icon(icon(false))
                    .build()
                {
                    Ok(t) => {
                        set_icon(&t, false);
                        tray = Some(t);
                        let how = if cfg!(windows) {
                            "Click the tray icon (it may be under ^ on the taskbar)"
                        } else {
                            "Use the menu bar / tray icon"
                        };
                        ui::toast(
                            "better-video is running",
                            &format!("{how} or press {} to record.", args.key),
                            None,
                        );
                        eprintln!(
                            "better-video is in your tray; recordings are saved to {}",
                            dir.display()
                        );
                    }
                    Err(e) => {
                        eprintln!("error: couldn't create tray icon: {e}");
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(UserEvent::Tray(TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            })) if cfg!(windows) => toggle = true,
            Event::UserEvent(UserEvent::Menu(e)) => {
                let id = e.id();
                if id == items.record.id() {
                    toggle = true;
                } else if id == items.again.id() {
                    again = true;
                } else if id == items.quit.id() {
                    quit.store(true, Ordering::Relaxed);
                } else if id == items.open_dir.id() {
                    ui::reveal(&dir);
                } else if id == items.last.id() {
                    if let Some(p) = &last {
                        open_path(p);
                    }
                } else if id == items.reveal.id() {
                    if let Some(p) = &last {
                        ui::reveal(p);
                    }
                } else if id == items.change_dir.id() {
                    let (proxy, start) = (proxy.clone(), dir.clone());
                    thread::spawn(move || {
                        let _ = proxy.send_event(UserEvent::Folder(ui::choose_folder(&start)));
                    });
                } else if id == items.copy.id() {
                    settings.copy = Some(items.copy.is_checked());
                    settings.save();
                }
            }
            Event::UserEvent(UserEvent::Folder(Some(new))) => match std::fs::create_dir_all(&new) {
                Ok(()) => {
                    dir = new;
                    items.show_dir(&dir);
                    settings.dir = Some(dir.clone());
                    settings.save();
                }
                Err(e) => eprintln!("error: can't use {}: {e}", new.display()),
            },
            Event::UserEvent(UserEvent::Started(result)) => {
                state = match result {
                    Ok(session) => {
                        if let Some(p) = session.picked() {
                            choices = p.choices;
                            settings.choices = Some(choices);
                            settings.last_area = Some(p.region);
                            settings.save();
                        }
                        items.show_recording();
                        tray.iter().for_each(|t| set_icon(t, true));
                        State::Recording(session)
                    }
                    Err(None) => {
                        items.show_idle(settings.last_area.is_some());
                        State::Idle
                    }
                    Err(Some(e)) => {
                        eprintln!("error: {e}");
                        ui::toast("Couldn't start recording", &e, None);
                        items.show_idle(settings.last_area.is_some());
                        State::Idle
                    }
                };
            }
            Event::UserEvent(UserEvent::Exported(result)) => {
                state = State::Idle;
                items.show_idle(settings.last_area.is_some());
                match result {
                    Ok(path) => {
                        items.show_last(&path);
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let size = std::fs::metadata(&path).map_or(0, |m| m.len());
                        let copied = if items.copy.is_checked() {
                            " · copied to clipboard"
                        } else {
                            ""
                        };
                        ui::toast(
                            &format!("Saved {name} ({})", human_size(size)),
                            &format!("In {}{copied}. Click to show it.", dir.display()),
                            Some(path.clone()),
                        );
                        if let Some(t) = &tray {
                            let _ = t.set_tooltip(Some(format!("better-video — saved {name}")));
                        }
                        last = Some(path);
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        ui::toast("Couldn't save the recording", &e, None);
                    }
                }
            }
            _ => {}
        }

        if let Some(k) = keys.as_mut() {
            // Ignore a hotkey still held from launching the app.
            let pressed = edge.pressed(key.held(&k()));
            toggle |= pressed && launched.elapsed().as_millis() > 500;
        }
        if let State::Recording(s) = &mut state {
            toggle |= s.finished();
        }
        let quitting = quit.load(Ordering::Relaxed);

        if toggle || again || quitting {
            state = match std::mem::replace(&mut state, State::Exporting) {
                State::Idle if quitting => State::Idle,
                State::Idle => {
                    let mut capture = base.clone();
                    capture.export.apply(&choices);
                    capture.export.copy = items.copy.is_checked();
                    match settings.last_area.filter(|_| again) {
                        Some(area) => capture.region = Some(area),
                        // Windows asks what to record; elsewhere it's the
                        // screen from --monitor/--region.
                        None => capture.select = cfg!(windows),
                    }
                    let output = fresh_output(&dir, choices.format);
                    items.show_busy(if capture.select {
                        "Choosing area…"
                    } else {
                        "Starting…"
                    });
                    let proxy = proxy.clone();
                    thread::spawn(move || {
                        let result = Session::start(&capture, output, choices.format)
                            .map(Box::new)
                            .map_err(|e| (!e.is::<ui::Cancelled>()).then(|| format!("{e:#}")));
                        let _ = proxy.send_event(UserEvent::Started(result));
                    });
                    State::Starting
                }
                State::Recording(session) => {
                    items.show_busy("Saving…");
                    tray.iter().for_each(|t| set_icon(t, false));
                    let proxy = proxy.clone();
                    thread::spawn(move || {
                        let result = (*session).finish().map_err(|e| format!("{e:#}"));
                        let _ = proxy.send_event(UserEvent::Exported(result));
                    });
                    State::Exporting
                }
                // Wait for the picker or the export; quitting resumes once
                // it lands.
                s @ (State::Starting | State::Exporting) => s,
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

/// Open a file with its default app.
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
