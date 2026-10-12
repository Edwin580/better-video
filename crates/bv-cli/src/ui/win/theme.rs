//! Design tokens and the small set of components the overlays share, so
//! the picker, the recording pill and notifications look like one app.

use super::canvas::{Canvas, Color, Rect, Weight};

pub const DIM: Color = Color::rgba(0, 0, 0, 115);
pub const SURFACE: Color = Color::rgb(30, 30, 34);
pub const TRACK: Color = Color::rgb(46, 46, 52);
pub const HOVER: Color = Color::rgba(255, 255, 255, 22);
pub const CHIP: Color = Color::rgb(236, 236, 240);
pub const CHIP_TEXT: Color = Color::rgb(24, 24, 27);
pub const BORDER: Color = Color::rgba(255, 255, 255, 26);
pub const TEXT: Color = Color::rgb(242, 242, 245);
pub const MUTED: Color = Color::rgb(152, 152, 162);
pub const RECORD: Color = Color::rgb(255, 69, 58);
pub const RECORD_HOVER: Color = Color::rgb(255, 99, 90);
#[cfg(feature = "tray")]
pub const SUCCESS: Color = Color::rgb(48, 209, 88);
pub const WHITE: Color = Color::rgb(255, 255, 255);

/// Pixel sizes at a monitor's UI scale (percent).
#[derive(Debug, Clone, Copy)]
pub struct Scale(pub i32);

impl Scale {
    pub fn px(self, v: i32) -> i32 {
        (v * self.0 + 50) / 100
    }

    pub fn f(self, v: f32) -> f32 {
        v * self.0 as f32 / 100.0
    }
}

/// A floating panel: soft shadow, surface, hairline border.
pub fn card(c: &mut Canvas, r: Rect, radius: i32, s: Scale) {
    c.shadow(r, radius, s.px(16), s.px(6), 110);
    c.round_rect(r, radius, SURFACE);
    c.round_stroke(r, radius, 1.0, BORDER);
}

/// One option in a [`segmented`] control.
#[derive(Debug, Clone, Copy)]
pub struct Seg<'a> {
    pub label: &'a str,
    pub on: bool,
    pub hover: bool,
}

const TEXT_PX: i32 = 13;

fn seg_widths(c: &mut Canvas, items: &[Seg], s: Scale) -> Vec<i32> {
    items
        .iter()
        .map(|i| c.measure(i.label, s.px(TEXT_PX), Weight::Semibold).0 + 2 * s.px(12))
        .collect()
}

/// Height of controls in the options bar.
pub fn control_height(s: Scale) -> i32 {
    s.px(32)
}

pub fn segmented_width(c: &mut Canvas, items: &[Seg], s: Scale) -> i32 {
    seg_widths(c, items, s).iter().sum::<i32>() + 2 * s.px(3)
}

/// A pill-shaped track with a light chip on the selected option.
/// Returns each option's rectangle, for hit-testing.
pub fn segmented(c: &mut Canvas, x: i32, y: i32, items: &[Seg], s: Scale) -> Vec<Rect> {
    let widths = seg_widths(c, items, s);
    let h = control_height(s);
    let track = Rect::new(x, y, widths.iter().sum::<i32>() + 2 * s.px(3), h);
    c.round_rect(track, s.px(9), TRACK);
    let mut out = Vec::new();
    let mut ix = x + s.px(3);
    for (item, w) in items.iter().zip(widths) {
        let r = Rect::new(ix, y + s.px(3), w, h - 2 * s.px(3));
        let color = if item.on {
            c.round_rect(r, s.px(7), CHIP);
            CHIP_TEXT
        } else {
            if item.hover {
                c.round_rect(r, s.px(7), HOVER);
            }
            if item.hover {
                TEXT
            } else {
                MUTED
            }
        };
        c.text_in(r, item.label, s.px(TEXT_PX), Weight::Semibold, color);
        out.push(r);
        ix += w;
    }
    out
}

/// Width of a [`record_button`] with `label`.
pub fn record_button_width(c: &mut Canvas, label: &str, s: Scale) -> i32 {
    c.measure(label, s.px(TEXT_PX), Weight::Semibold).0 + s.px(14 + 10 + 6 + 14)
}

/// Red primary button with a white dot.
pub fn record_button(c: &mut Canvas, r: Rect, label: &str, hover: bool, s: Scale) {
    c.round_rect(r, s.px(9), if hover { RECORD_HOVER } else { RECORD });
    let (cx, cy) = (r.x + s.px(14) + s.px(5), r.y + r.h / 2);
    c.circle(cx as f32, cy as f32, s.f(5.0), WHITE);
    let text = Rect::new(
        r.x + s.px(14 + 10 + 6),
        r.y,
        r.w - s.px(14 + 10 + 6 + 14),
        r.h,
    );
    let (tw, th) = c.measure(label, s.px(TEXT_PX), Weight::Semibold);
    c.text(
        text.x + (text.w - tw) / 2,
        text.y + (text.h - th) / 2,
        label,
        s.px(TEXT_PX),
        Weight::Semibold,
        WHITE,
    );
}

/// Round icon button with an ✕.
pub fn close_button(c: &mut Canvas, r: Rect, hover: bool, s: Scale) {
    if hover {
        c.round_rect(r, r.h / 2, HOVER);
    }
    let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
    let d = s.f(5.0);
    let color = if hover { TEXT } else { MUTED };
    c.line((cx - d, cy - d), (cx + d, cy + d), s.f(1.6), color);
    c.line((cx - d, cy + d), (cx + d, cy - d), s.f(1.6), color);
}

/// A thin vertical divider.
pub fn divider(c: &mut Canvas, x: i32, y: i32, h: i32) {
    c.fill(Rect::new(x, y, 1, h), BORDER);
}

/// Folder outline icon centred on (`cx`, `cy`).
pub fn folder_icon(c: &mut Canvas, cx: f32, cy: f32, s: Scale, color: Color) {
    let (w, h) = (s.f(14.0), s.f(10.0));
    let (x, y) = (cx - w / 2.0, cy - h / 2.0 + s.f(1.0));
    let t = s.f(1.4);
    let tab = s.f(5.0);
    c.line((x, y - s.f(2.0)), (x + tab, y - s.f(2.0)), t, color);
    c.line((x + tab, y - s.f(2.0)), (x + tab + s.f(2.0), y), t, color);
    c.line((x, y - s.f(2.0)), (x, y + h), t, color);
    c.line((x, y + h), (x + w, y + h), t, color);
    c.line((x + w, y + h), (x + w, y), t, color);
    c.line((x + w, y), (x + tab + s.f(2.0), y), t, color);
}

/// Monitor outline icon centred on (`cx`, `cy`).
pub fn monitor_icon(c: &mut Canvas, cx: f32, cy: f32, s: Scale, color: Color) {
    let (w, h) = (s.px(22), s.px(15));
    let screen = Rect::new(
        (cx - w as f32 / 2.0) as i32,
        (cy - h as f32 / 2.0 - s.f(2.0)) as i32,
        w,
        h,
    );
    c.round_stroke(screen, s.px(3), s.f(1.6), color);
    let base = cy + h as f32 / 2.0 + s.f(2.0);
    c.line((cx, base - s.f(2.0)), (cx, base), s.f(1.6), color);
    c.line(
        (cx - s.f(5.0), base + s.f(1.0)),
        (cx + s.f(5.0), base + s.f(1.0)),
        s.f(1.6),
        color,
    );
}

/// Small key label, e.g. "Esc". Returns its width.
pub fn keycap(c: &mut Canvas, x: i32, cy: i32, key: &str, s: Scale) -> i32 {
    let (tw, th) = c.measure(key, s.px(11), Weight::Semibold);
    let r = Rect::new(x, cy - th / 2 - s.px(2), tw + s.px(10), th + s.px(4));
    c.round_rect(r, s.px(5), TRACK);
    c.text_in(r, key, s.px(11), Weight::Semibold, TEXT);
    r.w
}

/// Shorten `s` with an ellipsis in the middle until it fits `max` pixels
/// (paths keep their start and their file name).
pub fn ellipsize(c: &mut Canvas, s: &str, size: i32, weight: Weight, max: i32) -> String {
    if c.measure(s, size, weight).0 <= max {
        return s.to_owned();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut keep = chars.len();
    while keep > 4 {
        keep -= 1;
        let head = keep / 2;
        let t: String = chars[..head]
            .iter()
            .chain(['…'].iter())
            .chain(&chars[chars.len() - (keep - head)..])
            .collect();
        if c.measure(&t, size, weight).0 <= max {
            return t;
        }
    }
    s.chars().take(4).collect()
}
