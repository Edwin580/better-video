//! Full-desktop overlay to choose what to record, modelled on macOS's
//! screen recording bar:
//!
//! 1. **Select** — click a screen, or drag out an area.
//! 2. **Adjust** — drag the handles to resize, drag inside to move, or
//!    drag elsewhere for a new area. A bar under the selection sets the
//!    format, size, click rings and zoom, and shows where the file goes.
//! 3. **Countdown** — a ring fills for each of 3, 2, 1, then the overlay
//!    closes and recording starts.
//!
//! Esc or right-click cancels at any point. Only the part of the overlay
//! that changed is redrawn and re-uploaded, so it stays light even across
//! several large monitors.

use std::{cell::RefCell, rc::Rc, time::Instant};

use bv_core::{monitor, CaptureRegion, Monitor, OutputFormat, Size};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, ReleaseCapture, SetCapture, VK_ESCAPE, VK_RETURN,
        },
        WindowsAndMessaging::{
            GetCursorPos, KillTimer, LoadCursorW, SetCursor, SetForegroundWindow, SetTimer,
            ShowWindow, HTCLIENT, IDC_CROSS, IDC_HAND, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS,
            IDC_SIZENWSE, IDC_SIZEWE, SW_SHOW, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
            WM_MOUSEMOVE, WM_RBUTTONUP, WM_SETCURSOR, WM_TIMER, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
            WS_EX_TOPMOST, WS_POPUP,
        },
    },
};

use super::{
    canvas::{Canvas, Color, Rect, Weight},
    close, create, mouse_pos, run_loop,
    theme::{self, Scale, Seg},
    Handler,
};
use crate::ui::{Choices, PickRequest, Picked, Quality, ZoomMode};

/// Smaller drags count as a click (select the whole screen).
const MIN_DRAG: i32 = 8;
/// Smallest area resizing allows.
const MIN_SIZE: i32 = 16;
/// Alpha 1 instead of 0: invisible, but still receives the mouse.
const CLEAR: Color = Color(0x0100_0000);
const EDGE: Color = Color::rgba(0, 0, 0, 90);

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
        bounds: area,
        canvas: Canvas::new(area.width as i32, area.height as i32),
        monitors: monitor::monitors(),
        cursor: (cursor.x, cursor.y),
        drag: None,
        edit: None,
        mode: Mode::Selecting,
        choices: req.choices,
        save_dir: req.save_dir.display().to_string(),
        countdown: req.countdown,
        buttons: Vec::new(),
        hover: Hit::Outside,
        hovered_screen: None,
        prev_ink: None,
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
        SetTimer(hwnd, TIMER_SHOW, 15, None);
    }
    run_loop();
    out.take()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Selecting,
    Adjusting(CaptureRegion),
    Countdown(CaptureRegion, Instant),
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

#[derive(Debug, Clone, Copy, PartialEq)]
enum Handle {
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

const HANDLES: [Handle; 8] = [
    Handle::NW,
    Handle::N,
    Handle::NE,
    Handle::E,
    Handle::SE,
    Handle::S,
    Handle::SW,
    Handle::W,
];

/// What's under the cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hit {
    Button(Button),
    Handle(Handle),
    Inside,
    Outside,
}

/// A move or resize in progress.
#[derive(Debug, Clone, Copy)]
struct Edit {
    hit: Hit,
    start: (i32, i32),
    region: CaptureRegion,
}

struct Picker {
    /// Virtual-desktop position of the canvas's top-left.
    origin: (i32, i32),
    bounds: CaptureRegion,
    canvas: Canvas,
    monitors: Vec<Monitor>,
    /// Cursor in screen coordinates.
    cursor: (i32, i32),
    drag: Option<(i32, i32)>,
    edit: Option<Edit>,
    mode: Mode,
    choices: Choices,
    save_dir: String,
    countdown: u32,
    /// Options-bar buttons as last drawn, in canvas coordinates.
    buttons: Vec<(Button, Rect)>,
    hover: Hit,
    hovered_screen: Option<usize>,
    /// What the last frame drew over the dim background.
    prev_ink: Option<Rect>,
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

    /// UI scale for the selection (or the monitor under the cursor).
    fn scale(&self) -> Scale {
        let p = self.selection().map_or(self.cursor, |r| {
            (r.x + r.width as i32 / 2, r.y + r.height as i32 / 2)
        });
        Scale(self.monitor_at(p).map_or(100, |(_, m)| m.scale as i32))
    }

    fn to_canvas(&self, r: CaptureRegion) -> Rect {
        Rect::new(
            r.x - self.origin.0,
            r.y - self.origin.1,
            r.width as i32,
            r.height as i32,
        )
    }

    fn handle_pos(hole: Rect, h: Handle) -> (i32, i32) {
        let (l, t, r, b) = (hole.x, hole.y, hole.right(), hole.bottom());
        let (cx, cy) = hole.center();
        match h {
            Handle::NW => (l, t),
            Handle::N => (cx, t),
            Handle::NE => (r, t),
            Handle::E => (r, cy),
            Handle::SE => (r, b),
            Handle::S => (cx, b),
            Handle::SW => (l, b),
            Handle::W => (l, cy),
        }
    }

    fn hit_test(&self, p: (i32, i32)) -> Hit {
        let (x, y) = (p.0 - self.origin.0, p.1 - self.origin.1);
        if let Some((b, _)) = self.buttons.iter().find(|(_, r)| r.contains(x, y)) {
            return Hit::Button(*b);
        }
        if let Mode::Adjusting(region) = self.mode {
            let hole = self.to_canvas(region);
            let reach = self.scale().px(9);
            for h in HANDLES {
                let (hx, hy) = Self::handle_pos(hole, h);
                if (x - hx).abs() <= reach && (y - hy).abs() <= reach {
                    return Hit::Handle(h);
                }
            }
            if hole.contains(x, y) {
                return Hit::Inside;
            }
        }
        Hit::Outside
    }

    /// Apply a move or resize drag to the area it started from, keeping it
    /// on the desktop and at least `MIN_SIZE`.
    fn edited(&self, e: Edit) -> CaptureRegion {
        let (dx, dy) = (self.cursor.0 - e.start.0, self.cursor.1 - e.start.1);
        let r = e.region;
        let b = self.bounds;
        let (bl, bt) = (b.x, b.y);
        let (br, bb) = (b.x + b.width as i32, b.y + b.height as i32);
        let (mut l, mut t) = (r.x, r.y);
        let (mut rr, mut bo) = (r.x + r.width as i32, r.y + r.height as i32);
        match e.hit {
            Hit::Inside => {
                let x = (l + dx).clamp(bl, br - r.width as i32);
                let y = (t + dy).clamp(bt, bb - r.height as i32);
                return CaptureRegion { x, y, ..r };
            }
            Hit::Handle(h) => {
                let (west, east, north, south) = match h {
                    Handle::N => (false, false, true, false),
                    Handle::S => (false, false, false, true),
                    Handle::E => (false, true, false, false),
                    Handle::W => (true, false, false, false),
                    Handle::NE => (false, true, true, false),
                    Handle::NW => (true, false, true, false),
                    Handle::SE => (false, true, false, true),
                    Handle::SW => (true, false, false, true),
                };
                if west {
                    l = (l + dx).clamp(bl, rr - MIN_SIZE);
                }
                if east {
                    rr = (rr + dx).clamp(l + MIN_SIZE, br);
                }
                if north {
                    t = (t + dy).clamp(bt, bo - MIN_SIZE);
                }
                if south {
                    bo = (bo + dy).clamp(t + MIN_SIZE, bb);
                }
            }
            Hit::Button(_) | Hit::Outside => {}
        }
        CaptureRegion {
            x: l,
            y: t,
            width: (rr - l) as u32,
            height: (bo - t) as u32,
        }
    }

    /// Redraw: erase what the last frame drew, draw this one, and upload
    /// only the union of the two.
    fn render(&mut self, hwnd: HWND) {
        let prev = self.prev_ink.take();
        match prev {
            Some(p) => self.canvas.fill(p, theme::DIM),
            None => self.canvas.clear(theme::DIM),
        }
        self.canvas.take_ink();
        self.buttons.clear();
        self.draw();
        let ink = self.canvas.take_ink();
        let dirty = match (prev, ink) {
            (Some(p), Some(i)) => Some(p.union(&i)),
            (Some(p), None) => Some(p),
            (None, _) => None,
        };
        self.canvas
            .present_dirty(hwnd, self.origin.0, self.origin.1, dirty, 255);
        self.prev_ink = ink;
    }

    fn draw(&mut self) {
        let Some(sel) = self.selection() else {
            return;
        };
        let s = self.scale();
        let hole = self.to_canvas(sel);
        self.canvas.fill(hole, CLEAR);
        // A hairline of shadow outside a crisp white edge reads on any
        // background.
        outline(&mut self.canvas, hole.inset(-s.px(2)), 1, EDGE);
        outline(
            &mut self.canvas,
            hole.inset(-s.px(1)),
            s.px(1).max(1),
            theme::WHITE,
        );

        match self.mode {
            Mode::Selecting => match self.dragged() {
                Some(r) => self.size_badge(hole, r, s),
                None => {
                    let hovered = self.monitor_at(self.cursor).map(|(i, m)| (i, m.region));
                    if let Some((i, m)) = hovered {
                        self.screen_hint(i, m, s);
                    }
                }
            },
            Mode::Adjusting(region) => {
                for h in HANDLES {
                    let (hx, hy) = Self::handle_pos(hole, h);
                    let (hx, hy) = (hx as f32, hy as f32);
                    self.canvas
                        .circle(hx, hy, s.f(6.0), Color::rgba(0, 0, 0, 70));
                    self.canvas.circle(hx, hy, s.f(4.5), theme::WHITE);
                }
                if self.edit.is_some() {
                    self.size_badge(hole, region, s);
                }
                self.options_bar(region, hole, s);
            }
            Mode::Countdown(_, started) => self.countdown_ring(hole, started, s),
        }
    }

    /// "1280 × 720" above the area's top-left corner (inside if no room).
    fn size_badge(&mut self, hole: Rect, r: CaptureRegion, s: Scale) {
        let label = format!("{} × {}", r.width, r.height);
        let (tw, th) = self.canvas.measure(&label, s.px(12), Weight::Semibold);
        let (w, h) = (tw + s.px(16), th + s.px(8));
        let y = if hole.y - h - s.px(8) >= 0 {
            hole.y - h - s.px(8)
        } else {
            hole.y + s.px(8)
        };
        let r = Rect::new(hole.x, y, w, h);
        self.canvas.round_rect(r, h / 2, theme::SURFACE);
        self.canvas
            .text_in(r, &label, s.px(12), Weight::Semibold, theme::TEXT);
    }

    /// Card in the middle of the hovered screen explaining what to do.
    fn screen_hint(&mut self, i: usize, m: CaptureRegion, s: Scale) {
        let mr = self.to_canvas(m);
        let c = &mut self.canvas;
        let title = format!("Screen {}", i + 1);
        let sub = format!("{} × {}", m.width, m.height);
        let line = "Click to record this screen, or drag to select an area";
        let (lw, lh) = c.measure(line, s.px(13), Weight::Regular);
        let (_, th) = c.measure(&title, s.px(15), Weight::Semibold);
        let (pad, icon) = (s.px(18), s.px(40));
        let w = (lw + 2 * pad).max(s.px(300));
        let h = pad + th.max(icon) + s.px(12) + lh + s.px(16) + s.px(18) + pad;
        let (cx, cy) = mr.center();
        let card = Rect::new(cx - w / 2, cy - h / 2, w, h);
        theme::card(c, card, s.px(14), s);

        let top = card.y + pad;
        let tile = Rect::new(card.x + pad, top, icon, icon);
        c.round_rect(tile, s.px(10), theme::TRACK);
        let (tx, ty) = tile.center();
        theme::monitor_icon(c, tx as f32, ty as f32, s, theme::TEXT);
        let text_x = tile.right() + s.px(12);
        c.text(
            text_x,
            top + s.px(1),
            &title,
            s.px(15),
            Weight::Semibold,
            theme::TEXT,
        );
        c.text(
            text_x,
            top + th + s.px(1),
            &sub,
            s.px(13),
            Weight::Regular,
            theme::MUTED,
        );
        let ly = top + th.max(icon) + s.px(12);
        c.text(
            card.x + pad,
            ly,
            line,
            s.px(13),
            Weight::Regular,
            theme::TEXT,
        );
        let ky = ly + lh + s.px(16) + s.px(9);
        let kw = theme::keycap(c, card.x + pad, ky, "Esc", s);
        let (_, mh) = c.measure("to cancel", s.px(12), Weight::Regular);
        c.text(
            card.x + pad + kw + s.px(6),
            ky - mh / 2,
            "to cancel",
            s.px(12),
            Weight::Regular,
            theme::MUTED,
        );
    }

    /// The options bar under the selection (inside its bottom edge if it
    /// doesn't fit on that screen), with a line about the output below.
    fn options_bar(&mut self, region: CaptureRegion, hole: Rect, s: Scale) {
        let ch = self.choices;
        let hover = match self.hover {
            Hit::Button(b) => Some(b),
            _ => None,
        };
        let seg = |b: Button, label: &'static str, on: bool| {
            (
                b,
                Seg {
                    label,
                    on,
                    hover: hover == Some(b),
                },
            )
        };
        let gif = OutputFormat::Gif;
        let mp4 = OutputFormat::Mp4;
        let groups: [Vec<(Button, Seg)>; 4] = [
            vec![
                seg(Button::Format(gif), "GIF", ch.format == gif),
                seg(Button::Format(mp4), "MP4", ch.format == mp4),
            ],
            Quality::ALL
                .iter()
                .map(|&q| seg(Button::Quality(q), q.label(), ch.quality == q))
                .collect(),
            vec![seg(Button::Clicks, "Show clicks", ch.clicks)],
            ZoomMode::ALL
                .iter()
                .map(|&z| seg(Button::Zoom(z), z.label(), ch.zoom == z))
                .collect(),
        ];
        let segs: Vec<Vec<Seg>> = groups
            .iter()
            .map(|g| g.iter().map(|(_, sg)| *sg).collect())
            .collect();

        let mid = (
            region.x + region.width as i32 / 2,
            region.y + region.height as i32 / 2,
        );
        let screen = self
            .monitor_at(mid)
            .map_or(self.canvas.bounds(), |(_, m)| self.to_canvas(m.region));

        let c = &mut self.canvas;
        let (gap, pad, h) = (s.px(8), s.px(8), theme::control_height(s));
        let zoom_label = "Zoom";
        let (zw, zh) = c.measure(zoom_label, s.px(12), Weight::Regular);
        let widths: Vec<i32> = segs
            .iter()
            .map(|g| theme::segmented_width(c, g, s))
            .collect();
        let record_w = theme::record_button_width(c, "Record", s);
        let bar_w = pad
            + widths.iter().sum::<i32>()
            + gap * (widths.len() as i32 - 1)
            + zw
            + s.px(6)
            + gap * 2
            + 1
            + record_w
            + s.px(4)
            + h
            + pad;
        let bar_h = h + 2 * pad;

        let out = Size::new(region.width, region.height).fit_width(ch.quality.width());
        let info = format!(
            "{}   ·   {} × {} {}, {} fps",
            self.save_dir,
            out.width,
            out.height,
            ch.format.extension().to_uppercase(),
            ch.quality.fps()
        );
        let info_h = s.px(30);

        let total_h = bar_h + s.px(8) + info_h;
        let mut y = hole.bottom() + s.px(14);
        if y + total_h > screen.bottom() - s.px(10) {
            y = hole.bottom() - total_h - s.px(16);
        }
        let x = (hole.x + hole.w / 2 - bar_w / 2).clamp(
            screen.x + s.px(10),
            (screen.right() - bar_w - s.px(10)).max(screen.x),
        );
        let bar = Rect::new(x, y, bar_w, bar_h);
        theme::card(c, bar, s.px(14), s);

        let mut gx = bar.x + pad;
        let cy = bar.y + pad;
        for (i, (g, items)) in groups.iter().zip(&segs).enumerate() {
            if i == 3 {
                let ty = cy + (h - zh) / 2;
                c.text(gx, ty, zoom_label, s.px(12), Weight::Regular, theme::MUTED);
                gx += zw + s.px(6);
            }
            let rects = theme::segmented(c, gx, cy, items, s);
            for ((b, _), r) in g.iter().zip(rects) {
                self.buttons.push((*b, r));
            }
            gx += widths[i] + gap;
        }
        theme::divider(c, gx, cy + s.px(6), h - s.px(12));
        gx += 1 + gap;
        let rec = Rect::new(gx, cy, record_w, h);
        theme::record_button(c, rec, "Record", hover == Some(Button::Record), s);
        self.buttons.push((Button::Record, rec));
        let x_btn = Rect::new(rec.right() + s.px(4), cy, h, h);
        theme::close_button(c, x_btn, hover == Some(Button::Cancel), s);
        self.buttons.push((Button::Cancel, x_btn));

        // Output line: where it's saved and what it will be.
        let line = Rect::new(bar.x, bar.bottom() + s.px(8), bar.w, info_h);
        c.round_rect(line, info_h / 2, theme::SURFACE);
        c.round_stroke(line, info_h / 2, 1.0, theme::BORDER);
        let ix = line.x + s.px(14);
        let icon_y = (line.y + info_h / 2) as f32;
        theme::folder_icon(c, (ix + s.px(7)) as f32, icon_y, s, theme::MUTED);
        let keys_w = s.px(170);
        let room = line.w - s.px(14 + 22) - keys_w - s.px(14);
        let info = theme::ellipsize(c, &info, s.px(12), Weight::Regular, room);
        let (_, ih) = c.measure(&info, s.px(12), Weight::Regular);
        let iy = line.y + (info_h - ih) / 2;
        c.text(
            ix + s.px(22),
            iy,
            &info,
            s.px(12),
            Weight::Regular,
            theme::TEXT,
        );
        let mut kx = line.right() - keys_w;
        let ky = line.y + info_h / 2;
        for (key, what) in [("Enter", "record"), ("Esc", "cancel")] {
            kx += theme::keycap(c, kx, ky, key, s) + s.px(5);
            let (ww, wh) = c.measure(what, s.px(12), Weight::Regular);
            c.text(
                kx,
                ky - wh / 2,
                what,
                s.px(12),
                Weight::Regular,
                theme::MUTED,
            );
            kx += ww + s.px(12);
        }
    }

    /// Big number in a ring that fills over each second.
    fn countdown_ring(&mut self, hole: Rect, started: Instant, s: Scale) {
        let elapsed = started.elapsed().as_secs_f32();
        let left = (self.countdown as f32 - elapsed).max(0.0);
        let n = left.ceil().max(1.0) as u32;
        let progress = 1.0 - (left - (n - 1) as f32);
        let (cx, cy) = hole.center();
        let (cx, cy) = (cx as f32, cy as f32);
        let r = s.f(46.0);
        let c = &mut self.canvas;
        let d = (2.0 * r) as i32;
        let disc = Rect::new((cx - r) as i32, (cy - r) as i32, d, d);
        c.shadow(disc, d / 2, s.px(16), s.px(6), 110);
        c.circle(cx, cy, r, theme::SURFACE);
        let ring = r - s.f(7.0);
        c.arc(cx, cy, ring, s.f(4.0), 0.0, 1.0, theme::TRACK);
        c.arc(cx, cy, ring, s.f(4.0), 0.0, progress, theme::WHITE);
        c.text_in(
            disc,
            &n.to_string(),
            s.px(34),
            Weight::Semibold,
            theme::TEXT,
        );
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
        self.mode = Mode::Countdown(region, Instant::now());
        unsafe { SetTimer(hwnd, TIMER_COUNT, 30, None) };
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

/// A border of thickness `t` just outside `r` (fills, so it's cheap even
/// around a whole screen).
fn outline(c: &mut Canvas, r: Rect, t: i32, color: Color) {
    c.fill(Rect::new(r.x - t, r.y - t, r.w + 2 * t, t), color);
    c.fill(Rect::new(r.x - t, r.bottom(), r.w + 2 * t, t), color);
    c.fill(Rect::new(r.x - t, r.y, t, r.h), color);
    c.fill(Rect::new(r.right(), r.y, t, r.h), color);
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
                        self.hovered_screen = self.monitor_at(self.cursor).map(|(i, _)| i);
                        self.render(hwnd);
                        unsafe {
                            ShowWindow(hwnd, SW_SHOW);
                            SetForegroundWindow(hwnd);
                        }
                    }
                    TIMER_COUNT => {
                        if let Mode::Countdown(region, started) = self.mode {
                            if started.elapsed().as_secs_f32() >= self.countdown as f32 {
                                self.finish(hwnd, Some(region));
                            } else {
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
                let hit = self
                    .edit
                    .map_or_else(|| self.hit_test(self.cursor), |e| e.hit);
                let cursor = match hit {
                    Hit::Button(_) => IDC_HAND,
                    Hit::Handle(Handle::N | Handle::S) => IDC_SIZENS,
                    Hit::Handle(Handle::E | Handle::W) => IDC_SIZEWE,
                    Hit::Handle(Handle::NW | Handle::SE) => IDC_SIZENWSE,
                    Hit::Handle(Handle::NE | Handle::SW) => IDC_SIZENESW,
                    Hit::Inside => IDC_SIZEALL,
                    Hit::Outside => return None,
                };
                unsafe { SetCursor(LoadCursorW(std::ptr::null_mut(), cursor)) };
                Some(1)
            }
            WM_MOUSEMOVE => {
                let p = screen(lparam);
                if p == self.cursor {
                    return Some(0);
                }
                self.cursor = p;
                match self.mode {
                    Mode::Selecting if self.drag.is_some() => self.render(hwnd),
                    Mode::Selecting => {
                        // Only the screen under the cursor matters.
                        let now = self.monitor_at(p).map(|(i, _)| i);
                        if now != self.hovered_screen {
                            self.hovered_screen = now;
                            self.render(hwnd);
                        }
                    }
                    Mode::Adjusting(_) => {
                        if let Some(e) = self.edit {
                            self.mode = Mode::Adjusting(self.edited(e));
                            self.render(hwnd);
                        } else {
                            let hit = self.hit_test(p);
                            if hit != self.hover {
                                self.hover = hit;
                                self.render(hwnd);
                            }
                        }
                    }
                    Mode::Countdown(..) => {}
                }
                Some(0)
            }
            WM_LBUTTONDOWN => {
                self.cursor = screen(lparam);
                match (self.mode, self.hit_test(self.cursor)) {
                    (Mode::Countdown(..), _) => {}
                    (Mode::Adjusting(_), Hit::Button(b)) => self.press(hwnd, b),
                    (Mode::Adjusting(region), hit @ (Hit::Handle(_) | Hit::Inside)) => {
                        self.edit = Some(Edit {
                            hit,
                            start: self.cursor,
                            region,
                        });
                        unsafe { SetCapture(hwnd) };
                        self.render(hwnd);
                    }
                    _ => {
                        // Start a (new) selection.
                        self.mode = Mode::Selecting;
                        self.hover = Hit::Outside;
                        self.drag = Some(self.cursor);
                        unsafe { SetCapture(hwnd) };
                        self.render(hwnd);
                    }
                }
                Some(0)
            }
            WM_LBUTTONUP => {
                self.cursor = screen(lparam);
                if self.edit.take().is_some() {
                    unsafe { ReleaseCapture() };
                    self.render(hwnd);
                } else if self.mode == Mode::Selecting && self.drag.is_some() {
                    let choice = self.selection();
                    self.drag = None;
                    unsafe { ReleaseCapture() };
                    if let Some(r) = choice {
                        self.mode = Mode::Adjusting(r);
                        self.hover = self.hit_test(self.cursor);
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
