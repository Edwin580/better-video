//! While recording: a red frame around the recorded area, and a small pill
//! with a pulsing dot, the elapsed time, Stop and Discard.
//!
//! Both windows are excluded from screen capture, so they never show up
//! in the recording even inside the area. The frame is transparent (and
//! so click-through) everywhere but its edge and is drawn once; only the
//! small pill redraws as time passes.

use std::{
    sync::{
        atomic::{AtomicU8, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use bv_core::{monitor, CaptureRegion};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT},
        WindowsAndMessaging::{
            DestroyWindow, KillTimer, PostMessageW, SetTimer, SetWindowDisplayAffinity, ShowWindow,
            IDC_ARROW, IDC_HAND, SW_SHOWNOACTIVATE, WM_CLOSE, WM_LBUTTONUP, WM_MOUSEMOVE, WM_TIMER,
            WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
};

use super::{
    canvas::{Canvas, Color, Rect, Weight},
    create, mouse_pos, run_loop,
    theme::{self, Scale},
    Handler, WM_MOUSELEAVE,
};
use crate::ui::StopRequest;

/// `WDA_EXCLUDEFROMCAPTURE` (Windows 10 2004+).
const EXCLUDE_FROM_CAPTURE: u32 = 0x11;

const NONE: u8 = 0;
const SAVE: u8 = 1;
const DISCARD: u8 = 2;

pub struct Indicator {
    request: Arc<AtomicU8>,
    windows: [isize; 2],
    thread: Option<JoinHandle<()>>,
}

impl Indicator {
    /// `None` if the windows can't be created or hidden from capture
    /// (older Windows), since a visible frame would end up in the clip.
    pub fn show(region: CaptureRegion) -> Option<Self> {
        let request = Arc::new(AtomicU8::new(NONE));
        let flag = request.clone();
        let (tx, rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("bv-indicator".into())
            .spawn(move || {
                let windows = open(region, flag);
                let ok = windows.is_some();
                let _ = tx.send(windows.map(|(a, b)| [a as isize, b as isize]));
                if ok {
                    run_loop();
                }
            })
            .ok()?;
        match rx.recv().ok().flatten() {
            Some(windows) => Some(Self {
                request,
                windows,
                thread: Some(thread),
            }),
            None => {
                let _ = thread.join();
                None
            }
        }
    }

    pub fn requested(&self) -> Option<StopRequest> {
        match self.request.load(Ordering::Relaxed) {
            SAVE => Some(StopRequest::Save),
            DISCARD => Some(StopRequest::Discard),
            _ => None,
        }
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        // The pill's window ends the thread's loop, so close it last.
        for hwnd in self.windows {
            unsafe { PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0) };
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Create the frame and the pill; `None` (with nothing left open) on
/// failure.
fn open(region: CaptureRegion, request: Arc<AtomicU8>) -> Option<(HWND, HWND)> {
    let (cx, cy) = (
        region.x + region.width as i32 / 2,
        region.y + region.height as i32 / 2,
    );
    let (screen, scale) = monitor::monitors()
        .iter()
        .find(|m| {
            let r = m.region;
            cx >= r.x && cy >= r.y && cx < r.x + r.width as i32 && cy < r.y + r.height as i32
        })
        .map_or((region, 100), |m| (m.region, m.scale as i32));
    let s = Scale(scale);

    let frame = create(
        "bv-frame",
        IDC_ARROW,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        WS_POPUP,
        (
            region.x,
            region.y,
            region.width as i32,
            region.height as i32,
        ),
        Box::new(Frame),
        false,
    );
    if frame.is_null() || unsafe { SetWindowDisplayAffinity(frame, EXCLUDE_FROM_CAPTURE) } == 0 {
        close_now(frame);
        return None;
    }
    let mut canvas = Canvas::new(region.width as i32, region.height as i32);
    let t = s.px(3);
    let (w, h) = (canvas.w, canvas.h);
    for edge in [
        Rect::new(0, 0, w, t),
        Rect::new(0, h - t, w, t),
        Rect::new(0, 0, t, h),
        Rect::new(w - t, 0, t, h),
    ] {
        canvas.fill(edge, theme::RECORD);
    }
    canvas.present(frame, region.x, region.y);
    unsafe { ShowWindow(frame, SW_SHOWNOACTIVATE) };

    let pill = Pill::new(s, request);
    let (pw, ph) = (pill.canvas.w, pill.canvas.h);
    // Under the area when there's room on that screen, else just inside.
    let screen_bottom = screen.y + screen.height as i32;
    let below = region.y + region.height as i32 + s.px(6);
    let py = if below + ph <= screen_bottom {
        below
    } else {
        region.y + region.height as i32 - ph - s.px(10)
    };
    let px = region.x + region.width as i32 / 2 - pw / 2;
    let pill_hwnd = create(
        "bv-pill",
        IDC_HAND,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        WS_POPUP,
        (px, py, pw, ph),
        Box::new(Pill {
            origin: (px, py),
            ..pill
        }),
        true,
    );
    if pill_hwnd.is_null()
        || unsafe { SetWindowDisplayAffinity(pill_hwnd, EXCLUDE_FROM_CAPTURE) } == 0
    {
        close_now(frame);
        close_now(pill_hwnd);
        return None;
    }
    unsafe { SetTimer(pill_hwnd, 1, 0, None) };
    Some((frame, pill_hwnd))
}

/// Destroy a window created on this thread (its handler can't run yet).
fn close_now(hwnd: HWND) {
    if !hwnd.is_null() {
        unsafe { DestroyWindow(hwnd) };
    }
}

/// The static frame: everything is drawn up front.
struct Frame;

impl Handler for Frame {
    fn message(&mut self, _: HWND, _: u32, _: WPARAM, _: LPARAM) -> Option<LRESULT> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Part {
    Stop,
    Discard,
}

struct Pill {
    origin: (i32, i32),
    canvas: Canvas,
    s: Scale,
    started: Instant,
    request: Arc<AtomicU8>,
    hover: Option<Part>,
    tracking: bool,
    /// Button rectangles in canvas coordinates.
    stop: Rect,
    discard: Rect,
}

/// Room around the capsule for its shadow.
const MARGIN: i32 = 12;

impl Pill {
    fn new(s: Scale, request: Arc<AtomicU8>) -> Self {
        let mut probe = Canvas::new(1, 1);
        let text = s.px(13);
        let (time_w, th) = probe.measure("00:00", text, Weight::Semibold);
        let (stop_w, _) = probe.measure("Stop", text, Weight::Semibold);
        let (disc_w, _) = probe.measure("Discard", text, Weight::Semibold);
        let h = (th + s.px(18)).max(s.px(40));
        let m = s.px(MARGIN);
        let mut x = m + s.px(14);
        x += s.px(10) + s.px(8) + time_w + s.px(12);
        let stop = Rect::new(
            x + s.px(10),
            m + s.px(5),
            s.px(12) + s.px(8) + stop_w + s.px(20),
            h - s.px(10),
        );
        let discard = Rect::new(stop.right() + s.px(2), stop.y, disc_w + s.px(20), stop.h);
        let w = discard.right() + s.px(6) - m;
        Self {
            origin: (0, 0),
            canvas: Canvas::new(w + 2 * m, h + 2 * m),
            s,
            started: Instant::now(),
            request,
            hover: None,
            tracking: false,
            stop,
            discard,
        }
    }

    fn render(&mut self, hwnd: HWND) {
        let s = self.s;
        let m = s.px(MARGIN);
        let c = &mut self.canvas;
        c.clear(Color(0));
        let body = Rect::new(m, m, c.w - 2 * m, c.h - 2 * m);
        theme::card(c, body, body.h / 2, s);

        let requested = self.request.load(Ordering::Relaxed);
        let secs = self.started.elapsed().as_secs_f32();
        // A slow breathing pulse on the dot while recording.
        let pulse = 0.6 + 0.4 * (secs * std::f32::consts::TAU / 1.6).cos().abs();
        let dot_x = (body.x + s.px(14) + s.px(5)) as f32;
        let mid = (body.y + body.h / 2) as f32;
        let dot = if requested == NONE {
            Color::rgba(255, 69, 58, (pulse * 255.0) as u8)
        } else {
            theme::MUTED
        };
        c.circle(dot_x, mid, s.f(5.0), dot);

        let text = s.px(13);
        let t = secs as u64;
        let label = format!("{}:{:02}", t / 60, t % 60);
        let (_, th) = c.measure(&label, text, Weight::Semibold);
        let tx = body.x + s.px(14) + s.px(10) + s.px(8);
        c.text(
            tx,
            body.y + (body.h - th) / 2,
            &label,
            text,
            Weight::Semibold,
            theme::TEXT,
        );
        theme::divider(
            c,
            self.stop.x - s.px(8),
            body.y + s.px(10),
            body.h - s.px(20),
        );

        let busy = requested != NONE;
        for (part, r) in [(Part::Stop, self.stop), (Part::Discard, self.discard)] {
            if !busy && self.hover == Some(part) {
                c.round_rect(r, r.h / 2, theme::HOVER);
            }
            let (label, color) = match (part, requested) {
                (Part::Stop, SAVE) => ("Saving…", theme::MUTED),
                (Part::Stop, _) => ("Stop", theme::TEXT),
                (Part::Discard, DISCARD) => ("Discarding…", theme::MUTED),
                (Part::Discard, _) if self.hover == Some(part) && !busy => {
                    ("Discard", theme::RECORD_HOVER)
                }
                (Part::Discard, _) => ("Discard", theme::MUTED),
            };
            let (lw, lh) = c.measure(label, text, Weight::Semibold);
            let mut lx = r.x + (r.w - lw) / 2;
            if part == Part::Stop && requested != SAVE {
                // Stop square before the label.
                let sq = s.px(10);
                lx = r.x + s.px(10) + sq + s.px(8);
                let icon = Rect::new(r.x + s.px(10), r.y + (r.h - sq) / 2, sq, sq);
                c.round_rect(icon, s.px(2), theme::TEXT);
            }
            c.text(
                lx,
                r.y + (r.h - lh) / 2,
                label,
                text,
                Weight::Semibold,
                color,
            );
        }
        c.present(hwnd, self.origin.0, self.origin.1);
    }

    fn part_at(&self, x: i32, y: i32) -> Option<Part> {
        if self.stop.contains(x, y) {
            Some(Part::Stop)
        } else if self.discard.contains(x, y) {
            Some(Part::Discard)
        } else {
            None
        }
    }
}

impl Handler for Pill {
    fn message(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_TIMER => {
                self.render(hwnd);
                if wparam == 1 {
                    unsafe {
                        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        // Fast enough for the pulse; the window is tiny.
                        SetTimer(hwnd, 2, 80, None);
                        KillTimer(hwnd, 1);
                    }
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
                let (x, y) = mouse_pos(lparam);
                let hover = self.part_at(x, y);
                if hover != self.hover {
                    self.hover = hover;
                    self.render(hwnd);
                }
                Some(0)
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.hover.take().is_some() {
                    self.render(hwnd);
                }
                Some(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_pos(lparam);
                let value = match self.part_at(x, y) {
                    Some(Part::Stop) => SAVE,
                    Some(Part::Discard) => DISCARD,
                    None => return Some(0),
                };
                // First choice wins.
                let _ = self.request.compare_exchange(
                    NONE,
                    value,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
                self.render(hwnd);
                Some(0)
            }
            _ => None,
        }
    }
}
