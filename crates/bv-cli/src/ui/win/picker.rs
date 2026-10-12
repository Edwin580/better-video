//! Full-desktop overlay to choose what to record: click a screen, or drag
//! out an area. Esc or right-click cancels.

use std::{cell::RefCell, rc::Rc};

use bv_core::{monitor, CaptureRegion, Monitor};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture, SetCapture, VK_ESCAPE},
        WindowsAndMessaging::{
            GetCursorPos, KillTimer, SetForegroundWindow, SetTimer, ShowWindow, IDC_CROSS, SW_SHOW,
            WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONUP, WM_TIMER,
            WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        },
    },
};

use super::{
    canvas::{Canvas, Color, Rect},
    close, create, mouse_pos, run_loop, Handler,
};

/// Smaller drags count as a click (select the whole screen).
const MIN_DRAG: i32 = 8;
const DIM: Color = Color::rgba(0, 0, 0, 110);
/// Alpha 1 instead of 0: invisible, but still receives the mouse.
const CLEAR: Color = Color(0x0100_0000);
const LABEL: Color = Color::rgba(24, 24, 27, 235);
const ACCENT: Color = Color::rgb(59, 130, 246);

/// Show the picker and wait for a choice. Must run on a thread without
/// other windows (its message loop ends when the picker closes).
pub fn run() -> Option<CaptureRegion> {
    let area = monitor::virtual_screen();
    let monitors = monitor::monitors();
    let out = Rc::new(RefCell::new(None));
    let mut cursor = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut cursor) };
    let picker = Picker {
        origin: (area.x, area.y),
        canvas: Canvas::new(area.width as i32, area.height as i32),
        monitors,
        cursor: (cursor.x, cursor.y),
        drag: None,
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
        SetTimer(hwnd, 1, 30, None);
    }
    run_loop();
    out.take()
}

struct Picker {
    /// Virtual-desktop position of the canvas's top-left.
    origin: (i32, i32),
    canvas: Canvas,
    monitors: Vec<Monitor>,
    /// Cursor in screen coordinates.
    cursor: (i32, i32),
    drag: Option<(i32, i32)>,
    out: Rc<RefCell<Option<CaptureRegion>>>,
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

    fn selection(&self) -> Option<CaptureRegion> {
        self.dragged()
            .or_else(|| self.monitor_at(self.cursor).map(|(_, m)| m.region))
    }

    /// UI scale of the monitor under the cursor.
    fn scale(&self) -> i32 {
        self.monitor_at(self.cursor)
            .map_or(100, |(_, m)| m.scale as i32)
    }

    fn to_canvas(&self, r: CaptureRegion) -> Rect {
        Rect::new(
            r.x - self.origin.0,
            r.y - self.origin.1,
            r.width as i32,
            r.height as i32,
        )
    }

    fn render(&mut self, hwnd: HWND) {
        let s = self.scale();
        let px = |v: i32| v * s / 100;
        let text = px(14);
        let dragging = self.dragged();
        let hole = self.selection().map(|r| self.to_canvas(r));
        let hovered = self
            .monitor_at(self.cursor)
            .map(|(i, m)| (i, m.region, self.to_canvas(m.region)));

        let c = &mut self.canvas;
        c.clear(DIM);
        let Some(hole) = hole else {
            c.present(hwnd, self.origin.0, self.origin.1);
            return;
        };
        c.fill(hole, CLEAR);
        c.outline(hole, px(2), ACCENT);

        match (dragging, hovered) {
            (Some(r), _) => {
                // Size readout under the area (inside it if there's no room).
                let label = format!("{} × {}", r.width, r.height);
                let below = hole.y + hole.h + px(22);
                let y = if below + px(16) < c.h {
                    below
                } else {
                    hole.y + hole.h - px(22)
                };
                c.pill(hole.x + hole.w / 2, y, &label, text, LABEL);
            }
            (None, Some((i, m, mr))) => {
                let (cx, cy) = mr.center();
                c.pill(
                    cx,
                    cy - px(24),
                    &format!("Screen {} · {} × {}", i + 1, m.width, m.height),
                    px(18),
                    LABEL,
                );
                c.pill(
                    cx,
                    cy + px(24),
                    "Click to record this screen  ·  Drag to select an area  ·  Esc to cancel",
                    text,
                    LABEL,
                );
            }
            (None, None) => {}
        }
        c.present(hwnd, self.origin.0, self.origin.1);
    }

    fn finish(&mut self, hwnd: HWND, choice: Option<CaptureRegion>) {
        *self.out.borrow_mut() = choice;
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
                if wparam == 1 {
                    // First tick: paint, then show and take focus.
                    unsafe {
                        KillTimer(hwnd, 1);
                        SetTimer(hwnd, 2, 50, None);
                    }
                    self.render(hwnd);
                    unsafe {
                        ShowWindow(hwnd, SW_SHOW);
                        SetForegroundWindow(hwnd);
                    }
                } else if unsafe { GetAsyncKeyState(VK_ESCAPE as i32) } < 0 {
                    // Polled too: a window opened from a hotkey may not get
                    // keyboard focus.
                    self.finish(hwnd, None);
                }
                Some(0)
            }
            WM_MOUSEMOVE => {
                let p = screen(lparam);
                if p != self.cursor {
                    self.cursor = p;
                    self.render(hwnd);
                }
                Some(0)
            }
            WM_LBUTTONDOWN => {
                self.cursor = screen(lparam);
                self.drag = Some(self.cursor);
                unsafe { SetCapture(hwnd) };
                self.render(hwnd);
                Some(0)
            }
            WM_LBUTTONUP => {
                self.cursor = screen(lparam);
                let choice = self.selection();
                self.finish(hwnd, choice);
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
            _ => None,
        }
    }
}
