//! A small notification card in the bottom-right corner of the main
//! screen. It fades in, stays while hovered, and fades out on its own;
//! clicking it shows the file in Explorer. Hidden from screen capture so
//! it never lands in a recording.

use std::{path::PathBuf, process::Command, thread, time::Instant};

use bv_core::monitor;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT},
        WindowsAndMessaging::{
            SetTimer, SetWindowDisplayAffinity, ShowWindow, SystemParametersInfoW, IDC_HAND,
            SPI_GETWORKAREA, SW_SHOWNOACTIVATE, WM_LBUTTONUP, WM_MOUSEMOVE, WM_TIMER,
            WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
};

use super::{
    canvas::{Canvas, Color, Rect, Weight},
    close, create, run_loop,
    theme::{self, Scale},
    Handler, WM_MOUSELEAVE,
};
use crate::ui::ToastKind;

const SHOW_SECS: f32 = 6.0;
const FADE_SECS: f32 = 0.18;
const EXCLUDE_FROM_CAPTURE: u32 = 0x11;
/// Room around the card for its shadow.
const MARGIN: i32 = 16;

/// Show a notification without blocking.
pub fn show(kind: ToastKind, title: String, body: String, reveal: Option<PathBuf>) {
    let _ = thread::Builder::new()
        .name("bv-toast".into())
        .spawn(move || open(kind, title, body, reveal));
}

fn open(kind: ToastKind, title: String, body: String, reveal: Option<PathBuf>) {
    let mut work = RECT {
        left: 0,
        top: 0,
        right: 1280,
        bottom: 720,
    };
    unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as *mut _, 0) };
    let s = Scale(
        monitor::monitors()
            .iter()
            .find(|m| m.primary)
            .map_or(100, |m| m.scale as i32),
    );
    let m = s.px(MARGIN);
    let (w, h) = (s.px(380) + 2 * m, s.px(80) + 2 * m);
    let (x, y) = (work.right - w - s.px(4), work.bottom - h - s.px(4));

    let toast = Toast {
        canvas: Canvas::new(w, h),
        origin: (x, y),
        s,
        kind,
        title,
        body,
        reveal,
        shown: Instant::now(),
        hovered: false,
        tracking: false,
        closing: None,
        drawn: false,
    };
    let hwnd = create(
        "bv-toast",
        IDC_HAND,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        WS_POPUP,
        (x, y, w, h),
        Box::new(toast),
        true,
    );
    if hwnd.is_null() {
        return;
    }
    unsafe {
        SetWindowDisplayAffinity(hwnd, EXCLUDE_FROM_CAPTURE);
        // Paint happens on the first tick; then ~60 Hz while fading.
        SetTimer(hwnd, 1, 16, None);
    }
    run_loop();
}

struct Toast {
    canvas: Canvas,
    origin: (i32, i32),
    s: Scale,
    kind: ToastKind,
    title: String,
    body: String,
    reveal: Option<PathBuf>,
    shown: Instant,
    hovered: bool,
    tracking: bool,
    /// When the fade-out started.
    closing: Option<Instant>,
    /// Painted and shown yet.
    drawn: bool,
}

impl Toast {
    fn draw(&mut self) {
        let s = self.s;
        let m = s.px(MARGIN);
        let c = &mut self.canvas;
        c.clear(Color(0));
        let card = Rect::new(m, m, c.w - 2 * m, c.h - 2 * m);
        theme::card(c, card, s.px(12), s);
        if self.hovered {
            c.round_stroke(card, s.px(12), 1.0, Color::rgba(255, 255, 255, 60));
        }

        // Icon badge.
        let r = s.f(15.0);
        let (ix, iy) = ((card.x + s.px(18)) as f32 + r, (card.y + card.h / 2) as f32);
        let (badge, mark) = match self.kind {
            ToastKind::Saved => (theme::SUCCESS, theme::WHITE),
            ToastKind::Error => (theme::RECORD, theme::WHITE),
            ToastKind::Info => (theme::TRACK, theme::TEXT),
        };
        c.circle(ix, iy, r, badge);
        let t = s.f(2.2);
        match self.kind {
            ToastKind::Saved => {
                let k = s.f(1.0);
                c.line((ix - 6.0 * k, iy), (ix - 2.0 * k, iy + 4.0 * k), t, mark);
                c.line(
                    (ix - 2.0 * k, iy + 4.0 * k),
                    (ix + 6.0 * k, iy - 4.5 * k),
                    t,
                    mark,
                );
            }
            ToastKind::Error => {
                c.line((ix, iy - s.f(6.0)), (ix, iy + s.f(1.5)), t, mark);
                c.circle(ix, iy + s.f(5.5), s.f(1.4), mark);
            }
            ToastKind::Info => {
                // The app's record glyph: a ring with a dot.
                c.arc(ix, iy, s.f(7.0), s.f(1.8), 0.0, 1.0, mark);
                c.circle(ix, iy, s.f(3.5), theme::RECORD);
            }
        }

        let tx = card.x + s.px(18) + 2 * r as i32 + s.px(14);
        let room = card.right() - tx - s.px(16);
        let title = theme::ellipsize(c, &self.title, s.px(14), Weight::Semibold, room);
        // Each body line is shortened on its own, so a long path doesn't
        // swallow the line after it.
        let body: Vec<String> = self
            .body
            .lines()
            .take(2)
            .map(|l| theme::ellipsize(c, l, s.px(12), Weight::Regular, room))
            .collect();
        let (_, th) = c.measure(&title, s.px(14), Weight::Semibold);
        let (_, lh) = c.measure("Ag", s.px(12), Weight::Regular);
        let body_h = body.len() as i32 * (lh + s.px(2));
        let mut y = card.y + (card.h - th - s.px(3) - body_h) / 2;
        c.text(tx, y, &title, s.px(14), Weight::Semibold, theme::TEXT);
        y += th + s.px(3);
        for line in &body {
            c.text(tx, y, line, s.px(12), Weight::Regular, theme::MUTED);
            y += lh + s.px(2);
        }
    }

    /// Opacity right now, from the fade-in, fade-out and hover state.
    fn opacity(&self) -> Option<u8> {
        let fade_in = (self.shown.elapsed().as_secs_f32() / FADE_SECS).min(1.0);
        let fade_out = match self.closing {
            Some(at) => 1.0 - at.elapsed().as_secs_f32() / FADE_SECS,
            None => 1.0,
        };
        if fade_out <= 0.0 {
            return None;
        }
        Some((fade_in.min(fade_out) * 255.0) as u8)
    }
}

impl Handler for Toast {
    fn message(&mut self, hwnd: HWND, msg: u32, _: WPARAM, _: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_TIMER => {
                let first = !self.drawn;
                self.drawn = true;
                if first {
                    self.draw();
                }
                // Start fading out after a while, unless hovered.
                if self.closing.is_none()
                    && !self.hovered
                    && self.shown.elapsed().as_secs_f32() > SHOW_SECS
                {
                    self.closing = Some(Instant::now());
                }
                match self.opacity() {
                    Some(a) => {
                        let (x, y) = self.origin;
                        self.canvas.present_dirty(hwnd, x, y, None, a);
                        if first {
                            unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
                        }
                    }
                    None => close(hwnd),
                }
                Some(0)
            }
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe { TrackMouseEvent(&mut tme) };
                    self.tracking = true;
                }
                if !self.hovered {
                    self.hovered = true;
                    // Hovering cancels a fade-out in progress.
                    self.closing = None;
                    self.draw();
                }
                Some(0)
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.hovered = false;
                // Give the reader a moment after moving away.
                self.shown = Instant::now() - std::time::Duration::from_secs_f32(SHOW_SECS - 2.0);
                self.draw();
                Some(0)
            }
            WM_LBUTTONUP => {
                if let Some(p) = &self.reveal {
                    reveal(p);
                }
                self.closing.get_or_insert_with(Instant::now);
                Some(0)
            }
            _ => None,
        }
    }
}

/// Open Explorer with `path` selected (or the folder itself).
pub fn reveal(path: &std::path::Path) {
    let result = if path.is_dir() {
        Command::new("explorer").arg(path).spawn()
    } else {
        // explorer wants `/select,` and the path as one argument.
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        Command::new("explorer").arg(arg).spawn()
    };
    if let Err(e) = result {
        eprintln!("error: couldn't open Explorer: {e}");
    }
}
