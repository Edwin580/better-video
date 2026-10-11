//! While recording: a red frame around the recorded area and a pill with
//! the elapsed time and a Stop button.
//!
//! The window is excluded from screen capture, so it never shows up in the
//! recording even though it sits inside the area. Everywhere except the
//! frame and the pill is fully transparent, which also makes it
//! click-through, so the recorded apps stay usable.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::Instant,
};

use bv_core::{monitor, CaptureRegion};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::WindowsAndMessaging::{
        PostMessageW, SetTimer, SetWindowDisplayAffinity, ShowWindow, IDC_HAND, SW_SHOWNOACTIVATE,
        WM_CLOSE, WM_LBUTTONUP, WM_TIMER, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_POPUP,
    },
};

use super::{
    canvas::{Canvas, Color, Rect},
    create, mouse_pos, run_loop, Handler,
};

/// `WDA_EXCLUDEFROMCAPTURE` (Windows 10 2004+).
const EXCLUDE_FROM_CAPTURE: u32 = 0x11;
const FRAME: Color = Color::rgb(220, 38, 38);
const PILL: Color = Color::rgb(24, 24, 27);

pub struct Indicator {
    stop: Arc<AtomicBool>,
    hwnd: isize,
    thread: Option<JoinHandle<()>>,
}

impl Indicator {
    /// `None` if the window can't be created or hidden from capture (older
    /// Windows), since a visible frame would end up in the recording.
    pub fn show(region: CaptureRegion) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let (tx, rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("bv-indicator".into())
            .spawn(move || {
                let hwnd = open(region, flag);
                let _ = tx.send(hwnd as isize);
                if !hwnd.is_null() {
                    run_loop();
                }
            })
            .ok()?;
        let hwnd = rx.recv().ok()?;
        if hwnd == 0 {
            let _ = thread.join();
            return None;
        }
        Some(Self {
            stop,
            hwnd,
            thread: Some(thread),
        })
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

impl Drop for Indicator {
    fn drop(&mut self) {
        unsafe { PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Create and show the window; null on failure.
fn open(region: CaptureRegion, stop: Arc<AtomicBool>) -> HWND {
    let monitors = monitor::monitors();
    let (cx, cy) = (
        region.x + region.width as i32 / 2,
        region.y + region.height as i32 / 2,
    );
    let screen = monitors
        .iter()
        .find(|m| {
            let r = m.region;
            cx >= r.x && cy >= r.y && cx < r.x + r.width as i32 && cy < r.y + r.height as i32
        })
        .map(|m| (m.region, m.scale as i32))
        .unwrap_or((region, 100));
    let (screen, scale) = screen;
    let px = |v: i32| v * scale / 100;

    // The pill goes under the area when there's room on that screen,
    // otherwise just inside its bottom edge.
    let (pw, ph) = (px(150), px(36));
    let screen_bottom = screen.y + screen.height as i32;
    let below = region.y + region.height as i32 + px(10);
    let pill_y = if below + ph <= screen_bottom {
        below
    } else {
        region.y + region.height as i32 - ph - px(14)
    };
    let pill_x = region.x + region.width as i32 / 2 - pw / 2;

    // Window covers the area and the pill.
    let left = region.x.min(pill_x);
    let top = region.y.min(pill_y);
    let right = (region.x + region.width as i32).max(pill_x + pw);
    let bottom = (region.y + region.height as i32).max(pill_y + ph);
    let (w, h) = (right - left, bottom - top);

    let ind = Window {
        origin: (left, top),
        canvas: Canvas::new(w, h),
        frame: Rect::new(
            region.x - left,
            region.y - top,
            region.width as i32,
            region.height as i32,
        ),
        pill: Rect::new(pill_x - left, pill_y - top, pw, ph),
        thickness: px(3),
        text: px(14),
        started: Instant::now(),
        stop,
    };
    let hwnd = create(
        "bv-indicator",
        IDC_HAND,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        WS_POPUP,
        (left, top, w, h),
        Box::new(ind),
        true,
    );
    if hwnd.is_null() {
        return hwnd;
    }
    unsafe {
        if SetWindowDisplayAffinity(hwnd, EXCLUDE_FROM_CAPTURE) == 0 {
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
            run_loop();
            return std::ptr::null_mut();
        }
        SetTimer(hwnd, 1, 0, None);
    }
    hwnd
}

struct Window {
    origin: (i32, i32),
    canvas: Canvas,
    /// The recorded area and the pill, in canvas coordinates.
    frame: Rect,
    pill: Rect,
    thickness: i32,
    text: i32,
    started: Instant,
    stop: Arc<AtomicBool>,
}

impl Window {
    fn render(&mut self, hwnd: HWND) {
        let c = &mut self.canvas;
        c.clear(Color(0));
        // Frame drawn just inside the edge so it's visible even for a
        // full screen; it isn't captured either way.
        let f = self.frame;
        let t = self.thickness;
        c.fill(Rect::new(f.x, f.y, f.w, t), FRAME);
        c.fill(Rect::new(f.x, f.y + f.h - t, f.w, t), FRAME);
        c.fill(Rect::new(f.x, f.y, t, f.h), FRAME);
        c.fill(Rect::new(f.x + f.w - t, f.y, t, f.h), FRAME);

        let secs = self.started.elapsed().as_secs();
        let label = if self.stop.load(Ordering::Relaxed) {
            "Stopping…".to_owned()
        } else {
            format!("●  {}:{:02}    ■  Stop", secs / 60, secs % 60)
        };
        let p = self.pill;
        c.round_rect(p, p.h / 2, PILL);
        let (tw, th) = c.measure(&label, self.text, true);
        c.text(
            p.x + (p.w - tw) / 2,
            p.y + (p.h - th) / 2,
            &label,
            self.text,
            true,
            [255, 255, 255],
        );
        c.present(hwnd, self.origin.0, self.origin.1);
    }
}

impl Handler for Window {
    fn message(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_TIMER => {
                if wparam == 1 {
                    self.render(hwnd);
                    unsafe {
                        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        SetTimer(hwnd, 1, 500, None);
                    }
                } else {
                    self.render(hwnd);
                }
                Some(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_pos(lparam);
                let p = self.pill;
                if x >= p.x && y >= p.y && x < p.x + p.w && y < p.y + p.h {
                    self.stop.store(true, Ordering::Relaxed);
                    self.render(hwnd);
                }
                Some(0)
            }
            _ => None,
        }
    }
}
