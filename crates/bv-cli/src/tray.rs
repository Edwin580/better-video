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
    /// Saved, or `Err(None)` if the clip was discarded.
    Exported(Result<PathBuf, Option<String>>),
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
        zoom: ui::ZoomMode::from_trigger(base.export.trigger()),
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
                        // Windows 11 hides new tray icons (sometimes without
                        // even a ^ to reach them), so lead with the hotkey and
                        // say where to turn the icon on.
                        let body = if cfg!(windows) {
                            format!(
                                "Press {} or click the tray icon to record.\n\
                                 No icon? Settings › Taskbar › Other system tray icons.",
                                args.key
                            )
                        } else {
                            format!("Press {} or use the menu bar icon to record.", args.key)
                        };
                        ui::toast(ui::ToastKind::Info, "better-video is running", &body, None);
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
                        ui::toast(ui::ToastKind::Error, "Couldn't start recording", &e, None);
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
                            "Copied to clipboard · "
                        } else {
                            ""
                        };
                        ui::toast(
                            ui::ToastKind::Saved,
                            &format!("Saved {name} ({})", human_size(size)),
                            &format!("{}\n{copied}Click to show it in the folder", dir.display()),
                            Some(path.clone()),
                        );
                        if let Some(t) = &tray {
                            let _ = t.set_tooltip(Some(format!("better-video — saved {name}")));
                        }
                        last = Some(path);
                    }
                    Err(None) => {}
                    Err(Some(e)) => {
                        eprintln!("error: {e}");
                        ui::toast(
                            ui::ToastKind::Error,
                            "Couldn't save the recording",
                            &e,
                            None,
                        );
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
                        let result = (*session)
                            .finish()
                            .map(|e| e.path)
                            .map_err(|e| (!e.is::<ui::Cancelled>()).then(|| format!("{e:#}")));
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

/// 32×32 tray glyph. Idle: a ring (light on a dark taskbar, dark on a
/// light one) around a red dot. Recording: a red disc with a stop square,
/// since clicking it stops.
fn icon(recording: bool) -> Icon {
    const N: u32 = 32;
    let fg = if light_taskbar() {
        [40, 40, 44]
    } else {
        [240, 240, 244]
    };
    let red = [255, 69, 58];
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    let c = N as f64 / 2.0;
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f64 + 0.5 - c, y as f64 + 0.5 - c);
            let d = dx.hypot(dy);
            // Anti-aliased coverage of a disc of radius r.
            let disc = |r: f64| (r + 0.5 - d).clamp(0.0, 1.0);
            let (rgb, a) = if recording {
                // Rounded stop square, 10 px wide, cut out in white.
                let q = (dx.abs() - 3.0).max(0.0).hypot((dy.abs() - 3.0).max(0.0)) - 2.0;
                let square = (0.5 - q).clamp(0.0, 1.0);
                let mix = |r: u8, w: u8| (r as f64 + (w as f64 - r as f64) * square) as u8;
                (
                    [mix(red[0], 255), mix(red[1], 255), mix(red[2], 255)],
                    disc(14.0),
                )
            } else {
                let ring = disc(14.0) - disc(11.0);
                let dot = disc(6.5);
                if dot > ring {
                    (red, dot)
                } else {
                    (fg, ring)
                }
            };
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (a * 255.0) as u8]);
        }
    }
    Icon::from_rgba(rgba, N, N).expect("valid icon dimensions")
}

/// Whether the Windows taskbar uses the light theme (it's dark by
/// default on Windows 11). Elsewhere, assume light.
fn light_taskbar() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
        };
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let value = wide("SystemUsesLightTheme");
        let mut data = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                &mut data as *mut u32 as *mut _,
                &mut size,
            )
        };
        status == 0 && data != 0
    }
    #[cfg(not(windows))]
    {
        true
    }
}
