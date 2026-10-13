//! The mouse pointer, drawn at export from the recorded cursor track
//! instead of being baked into the captured frames.
//!
//! Drawing it ourselves means it can be bigger than the OS one, glide
//! instead of jitter, squash a little on each click and get out of the way
//! when idle, all decided (and re-decided) at export time.
//!
//! Two rules keep it honest and the GIFs small:
//!
//! - Smoothing never moves a click: at each press the pointer is exactly
//!   where the real one was, with the correction faded in and out around
//!   it.
//! - A still pointer is bit-for-bit still. The spring snaps onto its
//!   target once it's close (like the zoom camera), the press squash
//!   returns to exactly 1, and fades end at exactly 0 or 1, so a parked
//!   pointer produces identical frames.

use std::ops::Range;

use crate::{
    cursor::CursorTrack,
    geometry::{Rect, Size},
};

/// How the drawn pointer looks and moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerStyle {
    /// Size relative to a standard (100% DPI) OS arrow at the output's
    /// scale. Zooming in enlarges it by the square root of the
    /// magnification: noticeably, but less than the picture.
    pub scale: f64,
    /// Roughly how long the pointer takes to catch up with the real one,
    /// in seconds; 0 follows the recorded path exactly.
    pub smoothing_secs: f64,
    /// Squash the pointer slightly while a button is held.
    pub bounce: bool,
    /// Fade the pointer out after it's been still for this many seconds.
    pub hide_after: Option<f64>,
}

impl Default for PointerStyle {
    fn default() -> Self {
        Self {
            scale: 1.5,
            smoothing_secs: 0.15,
            bounce: true,
            hide_after: None,
        }
    }
}

/// Simulation rate of the smoothing spring, independent of the export fps.
const STEP_HZ: f64 = 240.0;
/// Snap the spring onto the cursor within this many source pixels.
const SETTLE_PX: f64 = 0.25;
/// Pressed pointer scale is `1 - PRESS_SQUASH`.
const PRESS_SQUASH: f64 = 0.18;
/// Seconds to squash when pressed, and to spring back when released.
const PRESS_IN: f64 = 0.06;
const PRESS_OUT: f64 = 0.16;
/// Seconds to fade out when idle, and back in before it moves again.
const FADE_OUT: f64 = 0.3;
const FADE_IN: f64 = 0.12;
/// Moving less than this (source pixels) still counts as idle.
const IDLE_RADIUS: f64 = 1.0;

/// The arrow, in OS pixels at 100%, tip at the origin: the classic
/// left-edge-and-45°-wing shape with a tail.
const ARROW: [(f64, f64); 7] = [
    (0.0, 0.0),
    (0.0, 16.0),
    (3.8, 12.4),
    (6.5, 18.3),
    (8.9, 17.3),
    (6.3, 11.6),
    (11.2, 11.6),
];
/// Outline width around the white fill, in arrow units.
const OUTLINE: f64 = 1.25;
/// Shadow offset and softness, in arrow units, and its opacity.
const SHADOW_DX: f64 = 0.6;
const SHADOW_DY: f64 = 1.4;
const SHADOW_BLUR: f64 = 1.8;
const SHADOW_ALPHA: f64 = 0.35;
const FILL: [u8; 3] = [255, 255, 255];
const STROKE: [u8; 3] = [20, 20, 24];

/// Where and how the pointer is drawn at one instant. Equal states draw
/// identical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerState {
    /// Hotspot, in source pixels.
    pub x: f64,
    pub y: f64,
    /// Press squash, 1 when not pressed.
    pub scale: f64,
    /// Opacity, 0–1.
    pub alpha: f64,
}

/// Pointer path and effects for a whole clip, precomputed from the track.
pub struct Pointer<'a> {
    track: &'a CursorTrack,
    /// Output pixels per arrow unit, unzoomed and unsquashed.
    unit: f64,
    src_width: f64,
    smooth: Option<SmoothPath>,
    /// `(time, correction)` at each press: the smoothed path is pulled by
    /// `correction` so it lands exactly on the click.
    fixes: Vec<(f64, (f64, f64))>,
    /// Window over which a correction fades in and out.
    fix_secs: f64,
    /// `(down, up)` button presses.
    presses: Vec<(f64, f64)>,
    bounce: bool,
    /// `(start, end)` spans the pointer sits still; it's hidden from
    /// `start` until `end` (fading).
    hidden: Vec<(f64, f64)>,
}

impl<'a> Pointer<'a> {
    /// `src` is the recording's size and `out` the size it's scaled to
    /// (the recording inside any styled frame).
    pub fn new(style: &PointerStyle, track: &'a CursorTrack, src: Size, out: Size) -> Self {
        let unit = (out.width as f64 / src.width.max(1) as f64).max(0.6) * style.scale.max(0.1);
        let smooth = (style.smoothing_secs > 0.0)
            .then(|| SmoothPath::new(track, style.smoothing_secs))
            .filter(|s| !s.xs.is_empty());
        let presses = presses(track);
        let fixes = match &smooth {
            Some(s) => presses
                .iter()
                .filter_map(|&(down, _)| {
                    let (rx, ry) = track.position_at(down)?;
                    let (sx, sy) = s.at(down);
                    Some((down, (rx - sx, ry - sy)))
                })
                .collect(),
            None => Vec::new(),
        };
        let hidden = match style.hide_after {
            Some(after) => idle_spans(track, after.max(0.0)),
            None => Vec::new(),
        };
        Self {
            track,
            unit,
            src_width: src.width as f64,
            smooth,
            fixes,
            fix_secs: style.smoothing_secs.max(0.05),
            presses,
            bounce: style.bounce,
            hidden,
        }
    }

    /// The pointer at time `t`, or `None` while it's hidden.
    pub fn state(&self, t: f64) -> Option<PointerState> {
        let alpha = self.alpha(t);
        if alpha <= 0.0 {
            return None;
        }
        let (x, y) = match &self.smooth {
            Some(s) => {
                let (x, y) = s.at(t);
                let (dx, dy) = self.correction(t);
                (x + dx, y + dy)
            }
            None => self.track.position_at(t)?,
        };
        let scale = if self.bounce {
            1.0 - PRESS_SQUASH * self.press(t)
        } else {
            1.0
        };
        Some(PointerState { x, y, scale, alpha })
    }

    /// Output rows the pointer covers in a frame of `size` showing `view`.
    pub fn rows(&self, s: &PointerState, size: Size, view: Rect) -> Range<usize> {
        let (_, y0, _, y1) = self.bounds(s, size, view);
        let h = size.height as i64;
        (y0.clamp(0, h) as usize)..(y1.clamp(0, h) as usize)
    }

    /// Pixel box `(x0, y0, x1, y1)` (exclusive ends, unclipped) the
    /// pointer and its shadow can touch.
    fn bounds(&self, s: &PointerState, size: Size, view: Rect) -> (i64, i64, i64, i64) {
        let (hx, hy) = hotspot(s, size, view);
        let u = self.size(s, view);
        // In arrow units: the outline (and the shift that puts its tip on
        // the hotspot), then the shadow's offset and blur beyond it.
        let stroke = OUTLINE.max(1.0 / u);
        let reach = stroke + SHADOW_BLUR;
        let x0 = hx - reach * u;
        let y0 = hy - reach * u;
        let x1 = hx + (11.2 + 1.4 * stroke + SHADOW_DX + reach) * u;
        let y1 = hy + (18.3 + 1.9 * stroke + SHADOW_DY + reach) * u;
        (
            x0.floor() as i64 - 1,
            y0.floor() as i64 - 1,
            x1.ceil() as i64 + 1,
            y1.ceil() as i64 + 1,
        )
    }

    /// Draw the pointer in state `s` onto `frame` (RGBA, `size`), which
    /// shows the source region `view`.
    pub fn draw(&self, frame: &mut [u8], size: Size, view: Rect, s: &PointerState) {
        let (w, h) = (size.width as i64, size.height as i64);
        let (x0, y0, x1, y1) = self.bounds(s, size, view);
        let (hx, hy) = hotspot(s, size, view);
        // Output pixels per arrow unit.
        let u = self.size(s, view);
        // The outline is at least a pixel wide so a small pointer keeps it.
        let stroke = (OUTLINE * u).max(1.0);
        // Put the outline's tip, not the fill's, on the hotspot.
        let (ox, oy) = (0.4 * stroke, 0.9 * stroke);
        let blur = SHADOW_BLUR * u;
        for y in y0.max(0)..y1.min(h) {
            for x in x0.max(0)..x1.min(w) {
                let px = (x as f64 + 0.5 - hx - ox) / u;
                let py = (y as f64 + 0.5 - hy - oy) / u;
                // Signed distances in output pixels; negative inside.
                let d = arrow_distance(px, py) * u;
                let ds = arrow_distance(px - SHADOW_DX, py - SHADOW_DY) * u - stroke;
                let shadow = SHADOW_ALPHA * (1.0 - smoothstep(-blur, blur, ds));
                let outline = (0.5 - (d - stroke)).clamp(0.0, 1.0);
                let fill = (0.5 - d).clamp(0.0, 1.0);
                if shadow <= 0.0 && outline <= 0.0 {
                    continue;
                }
                let i = (y * w + x) as usize * 4;
                for c in 0..3 {
                    let mut v = frame[i + c] as f64;
                    v -= v * shadow * s.alpha;
                    v += (STROKE[c] as f64 - v) * outline * s.alpha;
                    v += (FILL[c] as f64 - v) * fill * s.alpha;
                    frame[i + c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }

    /// Output pixels per arrow unit in state `s`, showing `view`.
    fn size(&self, s: &PointerState, view: Rect) -> f64 {
        let zoom = (self.src_width / view.width).max(1.0);
        self.unit * s.scale * zoom.sqrt()
    }

    /// Pull toward the real cursor around each click: the full correction
    /// at the press, fading out within `fix_secs` either side (or blending
    /// straight into the next click's when they're close).
    fn correction(&self, t: f64) -> (f64, f64) {
        let f = &self.fixes;
        let w = self.fix_secs;
        let i = f.partition_point(|c| c.0 <= t);
        let fade = |dt: f64, e: (f64, f64)| {
            let k = 1.0 - smoothstep(0.0, 1.0, dt / w);
            (e.0 * k, e.1 * k)
        };
        match (i.checked_sub(1).map(|j| f[j]), f.get(i)) {
            (Some(a), Some(b)) if b.0 - a.0 <= 2.0 * w => {
                let k = smoothstep(0.0, 1.0, (t - a.0) / (b.0 - a.0));
                (
                    a.1 .0 + (b.1 .0 - a.1 .0) * k,
                    a.1 .1 + (b.1 .1 - a.1 .1) * k,
                )
            }
            (a, b) => {
                let a = a.map_or((0.0, 0.0), |a| fade(t - a.0, a.1));
                let b = b.map_or((0.0, 0.0), |b| fade(b.0 - t, b.1));
                (a.0 + b.0, a.1 + b.1)
            }
        }
    }

    /// How far into the press squash the pointer is, 0–1.
    fn press(&self, t: f64) -> f64 {
        let i = self.presses.partition_point(|p| p.0 <= t);
        let Some(&(down, up)) = i.checked_sub(1).map(|j| &self.presses[j]) else {
            return 0.0;
        };
        let squash = |held: f64| smoothstep(0.0, 1.0, held / PRESS_IN);
        if t < up {
            squash(t - down)
        } else {
            squash(up - down) * (1.0 - smoothstep(0.0, 1.0, (t - up) / PRESS_OUT))
        }
    }

    /// Opacity at `t`: fades out at the start of each idle span and back
    /// in just before the pointer moves again.
    fn alpha(&self, t: f64) -> f64 {
        let i = self.hidden.partition_point(|s| s.1 <= t);
        let Some(&(start, end)) = self.hidden.get(i) else {
            return 1.0;
        };
        let out = (start + FADE_OUT - t) / FADE_OUT;
        let back = (t - (end - FADE_IN)) / FADE_IN;
        smoothstep(0.0, 1.0, out.max(back))
    }
}

/// Hotspot of state `s` in output pixels.
fn hotspot(s: &PointerState, size: Size, view: Rect) -> (f64, f64) {
    let k = size.width as f64 / view.width;
    ((s.x - view.x) * k, (s.y - view.y) * k)
}

/// The track's position run through a critically damped spring, sampled
/// at [`STEP_HZ`].
struct SmoothPath {
    start: f64,
    xs: Vec<f64>,
    ys: Vec<f64>,
}

impl SmoothPath {
    fn new(track: &CursorTrack, secs: f64) -> Self {
        let (Some(first), Some(last)) = (track.samples.first(), track.samples.last()) else {
            return Self {
                start: 0.0,
                xs: Vec::new(),
                ys: Vec::new(),
            };
        };
        // Settles (to ~1%) in about `secs`.
        let omega = 6.6 / secs;
        let dt = 1.0 / STEP_HZ;
        let steps = ((last.t - first.t) * STEP_HZ).ceil() as usize + 1;
        let (mut x, mut y) = (Spring::new(first.x), Spring::new(first.y));
        let mut xs = Vec::with_capacity(steps);
        let mut ys = Vec::with_capacity(steps);
        for i in 0..steps {
            let t = first.t + i as f64 * dt;
            if i > 0 {
                let (tx, ty) = track.position_at(t).unwrap_or((x.x, y.x));
                x.step(tx, omega, dt);
                y.step(ty, omega, dt);
            }
            xs.push(x.x);
            ys.push(y.x);
        }
        Self {
            start: first.t,
            xs,
            ys,
        }
    }

    fn at(&self, t: f64) -> (f64, f64) {
        let f = ((t - self.start) * STEP_HZ).max(0.0);
        let i = (f as usize).min(self.xs.len() - 1);
        let j = (i + 1).min(self.xs.len() - 1);
        let k = (f - i as f64).min(1.0);
        let lerp = |v: &[f64]| {
            if v[i] == v[j] {
                v[i]
            } else {
                v[i] + (v[j] - v[i]) * k
            }
        };
        (lerp(&self.xs), lerp(&self.ys))
    }
}

/// Critically damped spring, stepped with its exact solution; snaps onto
/// the target once within [`SETTLE_PX`] and nearly still.
#[derive(Debug, Clone, Copy)]
struct Spring {
    x: f64,
    v: f64,
}

impl Spring {
    fn new(x: f64) -> Self {
        Self { x, v: 0.0 }
    }

    fn step(&mut self, target: f64, omega: f64, dt: f64) {
        let c1 = self.x - target;
        let c2 = self.v + omega * c1;
        let e = (-omega * dt).exp();
        self.x = target + (c1 + c2 * dt) * e;
        self.v = (c2 - omega * (c1 + c2 * dt)) * e;
        if (self.x - target).abs() < SETTLE_PX && self.v.abs() < SETTLE_PX * omega {
            self.x = target;
            self.v = 0.0;
        }
    }
}

/// `(down, up)` for each button press; `up` is infinite if it's still held
/// at the end.
fn presses(track: &CursorTrack) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut down = None;
    for s in &track.samples {
        match (s.pressed, down) {
            (true, None) => down = Some(s.t),
            (false, Some(d)) => {
                out.push((d, s.t));
                down = None;
            }
            _ => {}
        }
    }
    if let Some(d) = down {
        out.push((d, f64::INFINITY));
    }
    out
}

/// Spans during which the pointer should be hidden: from `after` seconds
/// into each stretch where it stays put (and no button changes), until it
/// moves again. A stretch that lasts to the end of the track never ends.
fn idle_spans(track: &CursorTrack, after: f64) -> Vec<(f64, f64)> {
    let s = &track.samples;
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let mut j = i;
        while j + 1 < s.len()
            && s[j + 1].pressed == s[i].pressed
            && (s[j + 1].x - s[i].x).hypot(s[j + 1].y - s[i].y) <= IDLE_RADIUS
        {
            j += 1;
        }
        let end = if j + 1 == s.len() {
            f64::INFINITY
        } else {
            // It's still at s[j]; it has moved by s[j + 1].
            s[j + 1].t
        };
        let start = s[i].t + after;
        if end - start > FADE_OUT + FADE_IN {
            out.push((start, end));
        }
        i = j + 1;
    }
    out
}

/// Signed distance from `(x, y)` to the arrow polygon, negative inside.
fn arrow_distance(x: f64, y: f64) -> f64 {
    let v = &ARROW;
    let mut d = (x - v[0].0).powi(2) + (y - v[0].1).powi(2);
    let mut sign = 1.0;
    let mut j = v.len() - 1;
    for i in 0..v.len() {
        let (ex, ey) = (v[j].0 - v[i].0, v[j].1 - v[i].1);
        let (wx, wy) = (x - v[i].0, y - v[i].1);
        let k = ((wx * ex + wy * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
        let (bx, by) = (wx - ex * k, wy - ey * k);
        d = d.min(bx * bx + by * by);
        let c = [y >= v[i].1, y < v[j].1, ex * wy > ey * wx];
        if c.iter().all(|&b| b) || c.iter().all(|&b| !b) {
            sign = -sign;
        }
        j = i;
    }
    sign * d.sqrt()
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Smallest range covering both (either may be empty or absent).
pub(crate) fn hull(a: Option<Range<usize>>, b: Option<Range<usize>>) -> Option<Range<usize>> {
    let a = a.filter(|r| !r.is_empty());
    let b = b.filter(|r| !r.is_empty());
    match (a, b) {
        (Some(a), Some(b)) => Some(a.start.min(b.start)..a.end.max(b.end)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::CursorSample;

    fn track(samples: &[(f64, f64, f64, bool)]) -> CursorTrack {
        CursorTrack {
            samples: samples
                .iter()
                .map(|&(t, x, y, pressed)| CursorSample { t, x, y, pressed })
                .collect(),
            ..Default::default()
        }
    }

    /// A pointer moving right across a 200×200 recording at 120 Hz,
    /// pressing at 1.0 s (while still moving) and stopping at 1.5 s.
    fn moving() -> CursorTrack {
        let mut s = Vec::new();
        for i in 0..=360 {
            let t = i as f64 / 120.0;
            let x = 20.0 + 100.0 * t.min(1.5);
            s.push((t, x, 50.0, (1.0..1.1).contains(&t)));
        }
        track(&s)
    }

    fn px(frame: &[u8], size: Size, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * size.width + x) * 4) as usize;
        [frame[i], frame[i + 1], frame[i + 2]]
    }

    fn render(p: &Pointer, size: Size, view: Rect, t: f64) -> Vec<u8> {
        let mut f = vec![128u8; (size.width * size.height * 4) as usize];
        if let Some(s) = p.state(t) {
            p.draw(&mut f, size, view, &s);
        }
        f
    }

    #[test]
    fn drawn_at_the_mapped_position() {
        let tr = track(&[(0.0, 150.0, 150.0, false)]);
        let size = Size::new(200, 200);
        let style = PointerStyle {
            smoothing_secs: 0.0,
            ..Default::default()
        };
        let p = Pointer::new(&style, &tr, size, size);
        // 2× zoom on the bottom-right quadrant: (150, 150) maps to (100, 100).
        let view = Rect {
            x: 100.0,
            y: 100.0,
            width: 100.0,
            height: 100.0,
        };
        let f = render(&p, size, view, 0.0);
        // White body just inside the tip, outline at the tip, nothing
        // up-left of it or far away.
        assert_eq!(px(&f, size, 104, 112), [255, 255, 255]);
        assert!(
            px(&f, size, 100, 101)[0] < 100,
            "{:?}",
            px(&f, size, 100, 101)
        );
        assert_eq!(px(&f, size, 96, 96), [128, 128, 128]);
        assert_eq!(px(&f, size, 150, 150), [128, 128, 128]);
        // Not where the unzoomed position would be.
        let unzoomed = render(&p, size, Rect::full(size), 0.0);
        assert_ne!(px(&unzoomed, size, 154, 162), [128, 128, 128]);
        assert_eq!(px(&unzoomed, size, 104, 112), [128, 128, 128]);
    }

    #[test]
    fn rows_cover_everything_drawn() {
        let tr = track(&[(0.0, 60.0, 40.0, false)]);
        let size = Size::new(200, 200);
        let p = Pointer::new(&PointerStyle::default(), &tr, size, size);
        let view = Rect::full(size);
        let f = render(&p, size, view, 0.0);
        let rows = p.rows(&p.state(0.0).unwrap(), size, view);
        for (i, v) in f.as_chunks::<4>().0.iter().enumerate() {
            if v[..3] != [128; 3] {
                assert!(rows.contains(&(i / 200)), "row {}", i / 200);
            }
        }
        assert!(rows.len() < 50, "{rows:?}");
    }

    #[test]
    fn smoothing_lands_exactly_on_clicks() {
        let tr = moving();
        let size = Size::new(200, 200);
        let p = Pointer::new(&PointerStyle::default(), &tr, size, size);
        let down = tr.clicks().next().unwrap();
        let s = p.state(down).unwrap();
        let (rx, ry) = tr.position_at(down).unwrap();
        assert!((s.x - rx).abs() < 1e-9 && (s.y - ry).abs() < 1e-9, "{s:?}");
        // And it really is smoothed elsewhere: it trails while moving.
        let s = p.state(0.5).unwrap();
        assert!(s.x < tr.position_at(0.5).unwrap().0 - 2.0, "{s:?}");
    }

    #[test]
    fn settles_to_identical_frames() {
        let tr = moving();
        let size = Size::new(200, 200);
        let p = Pointer::new(&PointerStyle::default(), &tr, size, size);
        let view = Rect::full(size);
        let a = p.state(2.2).unwrap();
        let b = p.state(2.9).unwrap();
        assert_eq!(a, b);
        assert_eq!((a.x, a.y, a.scale, a.alpha), (170.0, 50.0, 1.0, 1.0));
        assert_eq!(render(&p, size, view, 2.2), render(&p, size, view, 2.9));
    }

    #[test]
    fn squashes_while_pressed() {
        let tr = moving();
        let size = Size::new(200, 200);
        let p = Pointer::new(&PointerStyle::default(), &tr, size, size);
        assert_eq!(p.state(0.5).unwrap().scale, 1.0);
        let held = p.state(1.09).unwrap().scale;
        assert!((held - (1.0 - PRESS_SQUASH)).abs() < 1e-9, "{held}");
        assert_eq!(p.state(1.5).unwrap().scale, 1.0);
        let off = PointerStyle {
            bounce: false,
            ..Default::default()
        };
        let p = Pointer::new(&off, &tr, size, size);
        assert_eq!(p.state(1.09).unwrap().scale, 1.0);
    }

    #[test]
    fn hides_after_idle_when_enabled() {
        let tr = moving();
        let size = Size::new(200, 200);
        let style = PointerStyle {
            hide_after: Some(0.5),
            ..Default::default()
        };
        let p = Pointer::new(&style, &tr, size, size);
        assert!(p.state(1.2).is_some());
        assert!(p.state(2.5).is_none(), "hidden once idle");
        // Off by default.
        let p = Pointer::new(&PointerStyle::default(), &tr, size, size);
        assert!(p.state(2.9).is_some());

        // It comes back, fully, before it moves again.
        let tr = track(&[
            (0.0, 10.0, 10.0, false),
            (3.0, 10.0, 10.0, false),
            (3.1, 40.0, 10.0, false),
        ]);
        let p = Pointer::new(&style, &tr, size, size);
        assert!(p.state(2.0).is_none());
        assert_eq!(p.state(3.1).unwrap().alpha, 1.0);
        let fading = p.state(3.1 - FADE_IN / 2.0).unwrap().alpha;
        assert!(fading > 0.0 && fading < 1.0);
    }

    #[test]
    fn arrow_distance_signs() {
        assert!(arrow_distance(2.0, 8.0) < 0.0);
        assert!(arrow_distance(-1.0, 8.0) > 0.0);
        assert!((arrow_distance(-1.0, 8.0) - 1.0).abs() < 1e-9);
        assert!(arrow_distance(10.0, 2.0) > 0.0);
    }

    #[test]
    fn hull_of_ranges() {
        assert_eq!(hull(Some(2..4), Some(8..9)), Some(2..9));
        assert_eq!(hull(None, Some(3..5)), Some(3..5));
        assert_eq!(hull(Some(3..3), None), None);
    }
}
