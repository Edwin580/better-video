//! Full-desktop overlay to choose what to record, modelled on macOS's
//! screen recording bar:
//!
//! 1. **Select** — click a screen, or drag out an area.
//! 2. **Adjust** — an options bar under the selection: format, size,
//!    click rings, zoom, where the file goes, and Record. Dragging again
//!    picks a new area.
//! 3. **Countdown** — 3, 2, 1, then the overlay closes and recording starts.
//!
//! Esc or right-click cancels at any point.

use std::{cell::RefCell, rc::Rc};

use bv_core::{monitor, CaptureRegion, Monitor, OutputFormat, Size};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN,
        },
        WindowsAndMessaging::{
            GetCursorPos, KillTimer, LoadCursorW, SetCursor, SetForegroundWindow, SetTimer,
            ShowWindow, HTCLIENT, IDC_CROSS, IDC_HAND, SW_SHOW, WM_KEYDOWN, WM_LBUTTONDOWN,
            WM_LBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WS_EX_LAYERED,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
};

use super::{
    canvas::{Canvas, Color, Rect},
    close, create, mouse_pos, run_loop, Handler,
};
use crate::ui::{Choices, PickRequest, Picked, Quality, ZoomMode};

/// Smaller drags count as a click (select the whole screen).
const MIN_DRAG: i32 = 8;
const DIM: Color = Color::rgba(0, 0, 0, 110);
/// Alpha 1 instead of 0: invisible, but still receives the mouse.
const CLEAR: Color = Color(0x0100_0000);
const LABEL: Color = Color::rgba(24, 24, 27, 235);
const BAR: Color = Color::rgb(24, 24, 27);
const GROUP: Color = Color::rgb(44, 44, 50);
const ACCENT: Color = Color::rgb(59, 130, 246);
const RECORD: Color = Color::rgb(220, 38, 38);
const TEXT: [u8; 3] = [235, 235, 240];
const MUTED: [u8; 3] = [160, 160, 170];

const TIMER_SHOW: usize = 1;
const TIMER_POLL: usize = 2;
const TIMER_COUNT: usize = 3;

/// Show the picker and wait for a choice. Must run on a thread without
/// other windows (its message loop ends when the picker closes).
pub fn run(req: PickRequest) -> Option<Picked> {
    let area = monitor::virtual_screen();
    let out = Rc::new(RefCell::new(None));
    let mut cursor = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut cursor) };
    let picker = Picker {
        origin: (area.x, area.y),
        canvas: Canvas::new(area.width as i32, area.height as i32),
        monitors: monitor::monitors(),
        cursor: (cursor.x, cursor.y),
        drag: None,
        mode: Mode::Selecting,
        choices: req.choices,
        save_dir: req.save_dir.display().to_string(),
        countdown: req.countdown,
        buttons: Vec::new(),
        out: out.clone(),
    };
    let hwnd = create(
        "bv-picker",
        IDC_CROSS,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        WS_POPUP,
        (area.x, area.y, area.width as i32, area.height as i32),
        Box::new(picker),
        true,
    );
    if hwnd.is_null() {
        return None;
    }
    unsafe {
        // First paint happens on the first timer tick, before showing.
        SetTimer(hwnd, TIMER_SHOW, 30, None);
    }
    run_loop();
    out.take()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Selecting,
    Adjusting(CaptureRegion),
    /// Seconds left before recording.
    Countdown(CaptureRegion, u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Button {
    Format(OutputFormat),
    Quality(Quality),
    Clicks,
    Zoom(ZoomMode),
    Record,
    Cancel,
}

struct Picker {
    /// Virtual-desktop position of the canvas's top-left.
    origin: (i32, i32),
    canvas: Canvas,
    monitors: Vec<Monitor>,
    /// Cursor in screen coordinates.
    cursor: (i32, i32),
    drag: Option<(i32, i32)>,
    mode: Mode,
    choices: Choices,
    save_dir: String,
    countdown: u32,
    /// Options-bar buttons as last drawn, in canvas coordinates.
    buttons: Vec<(Button, Rect)>,
    out: Rc<RefCell<Option<Picked>>>,
}

impl Picker {
    fn monitor_at(&self, (x, y): (i32, i32)) -> Option<(usize, &Monitor)> {
        self.monitors.iter().enumerate().find(|(_, m)| {
            let r = m.region;
            x >= r.x && y >= r.y && x < r.x + r.width as i32 && y < r.y + r.height as i32
        })
    }

    /// The dragged area, if the drag is big enough to count.
    fn dragged(&self) -> Option<CaptureRegion> {
        let (sx, sy) = self.drag?;
        let (cx, cy) = self.cursor;
        let (w, h) = ((cx - sx).abs(), (cy - sy).abs());
        (w >= MIN_DRAG && h >= MIN_DRAG).then(|| CaptureRegion {
            x: sx.min(cx),
            y: sy.min(cy),
            width: w as u32,
            height: h as u32,
        })
    }

    /// What would be recorded right now.
    fn selection(&self) -> Option<CaptureRegion> {
        match self.mode {
            Mode::Adjusting(r) | Mode::Countdown(r, _) => Some(r),
            Mode::Selecting => self
                .dragged()
                .or_else(|| self.monitor_at(self.cursor).map(|(_, m)| m.region)),
        }
    }

    /// UI scale of the monitor at a screen point.
    fn scale_at(&self, p: (i32, i32)) -> i32 {
        self.monitor_at(p).map_or(100, |(_, m)| m.scale as i32)
    }

    fn to_canvas(&self, r: CaptureRegion) -> Rect {
        Rect::new(
            r.x - self.origin.0,
            r.y - self.origin.1,
            r.width as i32,
            r.height as i32,
        )
    }

    fn button_at(&self, (x, y): (i32, i32)) -> Option<Button> {
        let (x, y) = (x - self.origin.0, y - self.origin.1);
        self.buttons
            .iter()
            .find(|(_, r)| x >= r.x && y >= r.y && x < r.x + r.w && y < r.y + r.h)
            .map(|(b, _)| *b)
    }

    fn render(&mut self, hwnd: HWND) {
        let hole = self.selection().map(|r| (r, self.to_canvas(r)));
        let hovered = self
            .monitor_at(self.cursor)
            .map(|(i, m)| (i, m.region, self.to_canvas(m.region)));
        let s = match hole {
            Some((r, _)) => self.scale_at((r.x + r.width as i32 / 2, r.y + r.height as i32 / 2)),
            None => self.scale_at(self.cursor),
        };
        let px = |v: i32| v * s / 100;
        self.buttons.clear();

        self.canvas.clear(DIM);
        if let Some((region, hole)) = hole {
            self.canvas.fill(hole, CLEAR);
            self.canvas.outline(hole, px(2), ACCENT);
            match self.mode {
                Mode::Selecting => match (self.dragged(), hovered) {
                    (Some(r), _) => self.size_readout(hole, r, px),
                    (None, Some((i, m, mr))) => self.screen_hint(i, m, mr, px),
                    (None, None) => {}
                },
                Mode::Adjusting(_) => self.options_bar(region, hole, px),
                Mode::Countdown(_, n) => {
                    let (cx, cy) = hole.center();
                    self.canvas.pill(cx, cy, &n.to_string(), px(56), LABEL);
                }
            }
        }
        self.canvas.present(hwnd, self.origin.0, self.origin.1);
    }

    fn size_readout(&mut self, hole: Rect, r: CaptureRegion, px: impl Fn(i32) -> i32) {
        let label = format!("{} × {}", r.width, r.height);
        let below = hole.y + hole.h + px(22);
        let y = if below + px(16) < self.canvas.h {
            below
        } else {
            hole.y + hole.h - px(22)
        };
        self.canvas
            .pill(hole.x + hole.w / 2, y, &label, px(14), LABEL);
    }

    fn screen_hint(&mut self, i: usize, m: CaptureRegion, mr: Rect, px: impl Fn(i32) -> i32) {
        let (cx, cy) = mr.center();
        self.canvas.pill(
            cx,
            cy - px(24),
            &format!("Screen {} · {} × {}", i + 1, m.width, m.height),
            px(18),
            LABEL,
        );
        self.canvas.pill(
            cx,
            cy + px(24),
            "Click to record this screen  ·  Drag to select an area  ·  Esc to cancel",
            px(14),
            LABEL,
        );
    }

    /// The options bar: under the selection if it fits on that screen,
    /// otherwise inside its bottom edge.
    fn options_bar(&mut self, region: CaptureRegion, hole: Rect, px: impl Fn(i32) -> i32) {
        let c = self.choices;
        let sizes: Vec<_> = Quality::ALL
            .iter()
            .map(|&q| (Button::Quality(q), q.label(), c.quality == q))
            .collect();
        // "Zoom off" reads better than a bare "Off" next to the other groups.
        let zooms: Vec<_> = ZoomMode::ALL
            .iter()
            .map(|&z| {
                let label = if z == ZoomMode::Off {
                    "Zoom off"
                } else {
                    z.label()
                };
                (Button::Zoom(z), label, c.zoom == z)
            })
            .collect();
        let groups: [&[(Button, &str, bool)]; 5] = [
            &[
                (
                    Button::Format(OutputFormat::Gif),
                    "GIF",
                    c.format == OutputFormat::Gif,
                ),
                (
                    Button::Format(OutputFormat::Mp4),
                    "MP4",
                    c.format == OutputFormat::Mp4,
                ),
            ],
            &sizes,
            &[(Button::Clicks, "Show clicks", c.clicks)],
            &zooms,
            &[
                (Button::Record, "●  Record", true),
                (Button::Cancel, "Cancel", false),
            ],
        ];
        let (text, pad, bh, gap) = (px(14), px(14), px(32), px(8));

        // Measure: each button is its text plus padding; groups sit in a
        // row with gaps, inside the bar's padding.
        let mut widths = Vec::new();
        for g in groups {
            widths.push(
                g.iter()
                    .map(|(_, label, _)| self.canvas.measure(label, text, true).0 + 2 * pad)
                    .collect::<Vec<_>>(),
            );
        }
        let bar_w = widths
            .iter()
            .map(|g| g.iter().sum::<i32>() + 2 * px(3))
            .sum::<i32>()
            + gap * (groups.len() as i32 + 1);
        let bar_h = bh + 2 * px(3) + 2 * gap;

        let out = Size::new(region.width, region.height).fit_width(c.quality.width());
        let format = c.format.extension().to_uppercase();
        let info = format!(
            "{} × {} {format}, {} fps   ·   Saves to {}   ·   Enter to record, drag to reselect, Esc to cancel",
            out.width,
            out.height,
            c.quality.fps(),
            self.save_dir
        );
        let info_h = px(30);

        // Position, clamped to the screen the selection is on.
        let screen = self
            .monitor_at((
                region.x + region.width as i32 / 2,
                region.y + region.height as i32 / 2,
            ))
            .map_or(Rect::new(0, 0, self.canvas.w, self.canvas.h), |(_, m)| {
                self.to_canvas(m.region)
            });
        let total_h = bar_h + gap + info_h;
        let mut y = hole.y + hole.h + px(16);
        if y + total_h > screen.y + screen.h - px(8) {
            y = hole.y + hole.h - total_h - px(16);
        }
        let x = (hole.x + hole.w / 2 - bar_w / 2).clamp(
            screen.x + px(8),
            (screen.x + screen.w - bar_w - px(8)).max(screen.x),
        );
        let bar = Rect::new(x, y, bar_w, bar_h);
        self.canvas.round_rect(bar, px(12), BAR);

        let mut gx = bar.x + gap;
        let by = bar.y + gap;
        for (g, ws) in groups.iter().zip(&widths) {
            let gw = ws.iter().sum::<i32>() + 2 * px(3);
            let grect = Rect::new(gx, by, gw, bh + 2 * px(3));
            self.canvas.round_rect(grect, px(9), GROUP);
            let mut bx = gx + px(3);
            for ((button, label, on), w) in g.iter().zip(ws) {
                let r = Rect::new(bx, by + px(3), *w, bh);
                let fill = match button {
                    Button::Record => Some(RECORD),
                    _ if *on => Some(ACCENT),
                    _ => None,
                };
                if let Some(f) = fill {
                    self.canvas.round_rect(r, px(7), f);
                }
                let (tw, th) = self.canvas.measure(label, text, true);
                let color = if fill.is_some() || *button == Button::Cancel {
                    TEXT
                } else {
                    MUTED
                };
                self.canvas.text(
                    r.x + (r.w - tw) / 2,
                    r.y + (r.h - th) / 2,
                    label,
                    text,
                    true,
                    color,
                );
                self.buttons.push((*button, r));
                bx += w;
            }
            gx += gw + gap;
        }

        let (cx, _) = bar.center();
        self.canvas
            .pill(cx, bar.y + bar_h + gap + info_h / 2, &info, px(12), LABEL);
    }

    fn press(&mut self, hwnd: HWND, button: Button) {
        let c = &mut self.choices;
        match button {
            Button::Format(f) => c.format = f,
            Button::Quality(q) => c.quality = q,
            Button::Clicks => c.clicks = !c.clicks,
            Button::Zoom(z) => c.zoom = z,
            Button::Record => return self.start_countdown(hwnd),
            Button::Cancel => return self.finish(hwnd, None),
        }
        self.render(hwnd);
    }

    fn start_countdown(&mut self, hwnd: HWND) {
        let Mode::Adjusting(region) = self.mode else {
            return;
        };
        if self.countdown == 0 {
            return self.finish(hwnd, Some(region));
        }
        self.mode = Mode::Countdown(region, self.countdown);
        unsafe { SetTimer(hwnd, TIMER_COUNT, 1000, None) };
        self.render(hwnd);
    }

    fn finish(&mut self, hwnd: HWND, region: Option<CaptureRegion>) {
        *self.out.borrow_mut() = region.map(|region| Picked {
            region,
            choices: self.choices,
        });
        unsafe { ReleaseCapture() };
        close(hwnd);
    }
}

impl Handler for Picker {
    fn message(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let screen = |lp| {
            let (x, y) = mouse_pos(lp);
            (x + self.origin.0, y + self.origin.1)
        };
        match msg {
            WM_TIMER => {
                match wparam {
                    TIMER_SHOW => {
                        // Paint first, then show and take focus.
                        unsafe {
                            KillTimer(hwnd, TIMER_SHOW);
                            SetTimer(hwnd, TIMER_POLL, 50, None);
                        }
                        self.render(hwnd);
                        unsafe {
                            ShowWindow(hwnd, SW_SHOW);
                            SetForegroundWindow(hwnd);
                        }
                    }
                    TIMER_COUNT => {
                        if let Mode::Countdown(region, n) = self.mode {
                            if n <= 1 {
                                self.finish(hwnd, Some(region));
                            } else {
                                self.mode = Mode::Countdown(region, n - 1);
                                self.render(hwnd);
                            }
                        }
                    }
                    // Esc is polled too: a window opened from a hotkey may
                    // not get keyboard focus.
                    _ if unsafe { GetAsyncKeyState(VK_ESCAPE as i32) } < 0 => {
                        self.finish(hwnd, None);
                    }
                    _ => {}
                }
                Some(0)
            }
            WM_SETCURSOR if (lparam & 0xFFFF) as u32 == HTCLIENT => {
                if self.button_at(self.cursor).is_some() {
                    unsafe { SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_HAND)) };
                    return Some(1);
                }
                None
            }
            WM_MOUSEMOVE => {
                let p = screen(lparam);
                if p != self.cursor {
                    self.cursor = p;
                    // Only the selecting stage follows the mouse.
                    if self.mode == Mode::Selecting {
                        self.render(hwnd);
                    }
                }
                Some(0)
            }
            WM_LBUTTONDOWN => {
                self.cursor = screen(lparam);
                let button = self.button_at(self.cursor);
                match (self.mode, button) {
                    (Mode::Countdown(..), _) => {}
                    (Mode::Adjusting(_), Some(b)) => self.press(hwnd, b),
                    _ => {
                        // Start a (new) selection.
                        self.mode = Mode::Selecting;
                        self.drag = Some(self.cursor);
                        unsafe { SetCapture(hwnd) };
                        self.render(hwnd);
                    }
                }
                Some(0)
            }
            WM_LBUTTONUP => {
                self.cursor = screen(lparam);
                if self.mode == Mode::Selecting && self.drag.is_some() {
                    let choice = self.selection();
                    self.drag = None;
                    unsafe { ReleaseCapture() };
                    if let Some(r) = choice {
                        self.mode = Mode::Adjusting(r);
                    }
                    self.render(hwnd);
                }
                Some(0)
            }
            WM_RBUTTONUP => {
                self.finish(hwnd, None);
                Some(0)
            }
            WM_KEYDOWN if wparam == VK_ESCAPE as usize => {
                self.finish(hwnd, None);
                Some(0)
            }
            WM_KEYDOWN if wparam == VK_RETURN as usize => {
                self.start_countdown(hwnd);
                Some(0)
            }
            _ => None,
        }
    }
}
