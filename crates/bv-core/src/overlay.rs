//! Effects drawn on top of exported frames.

use crate::{
    cursor::CursorTrack,
    geometry::{Rect, Size},
};

const RING_SECS: f64 = 0.5;
const RING_COLOR: [u8; 3] = [255, 196, 0];

/// Expanding, fading ring at each click, so viewers can see what was
/// clicked even without zoom.
pub struct ClickRings {
    /// (time, x, y) in source coordinates, sorted by time.
    clicks: Vec<(f64, f64, f64)>,
}

impl ClickRings {
    pub fn new(track: &CursorTrack) -> Self {
        let mut clicks: Vec<_> = track
            .clicks()
            .filter_map(|t| track.position_at(t).map(|(x, y)| (t, x, y)))
            .collect();
        clicks.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self { clicks }
    }

    /// Whether any ring is visible at time `t`.
    pub fn active_at(&self, t: f64) -> bool {
        let first = self.clicks.partition_point(|c| c.0 < t - RING_SECS);
        self.clicks.get(first).is_some_and(|c| c.0 <= t)
    }

    /// Draw the rings active at time `t` onto `frame` (RGBA, `size`), which
    /// shows the source region `view`.
    pub fn draw(&self, frame: &mut [u8], size: Size, view: Rect, t: f64) {
        let first = self.clicks.partition_point(|c| c.0 < t - RING_SECS);
        let scale = size.width as f64 / view.width;
        // Ring size is relative to the output, not the zoom level.
        let px = (size.width as f64 / 1280.0).max(0.5);
        for &(ct, x, y) in self.clicks[first..].iter().take_while(|c| c.0 <= t) {
            let p = (t - ct) / RING_SECS;
            let ease = 1.0 - (1.0 - p) * (1.0 - p);
            let radius = (10.0 + 24.0 * ease) * px;
            let cx = (x - view.x) * scale;
            let cy = (y - view.y) * scale;
            let alpha = 1.0 - p * p;
            // Dark outline under the coloured ring keeps it visible on any
            // background, including one the same colour as the ring.
            draw_ring(frame, size, cx, cy, radius, 7.0 * px, [0; 3], 0.8 * alpha);
            draw_ring(frame, size, cx, cy, radius, 3.0 * px, RING_COLOR, alpha);
        }
    }
}

/// Anti-aliased ring blended onto an RGBA frame.
#[allow(clippy::too_many_arguments)]
fn draw_ring(
    frame: &mut [u8],
    size: Size,
    cx: f64,
    cy: f64,
    radius: f64,
    thickness: f64,
    color: [u8; 3],
    alpha: f64,
) {
    let (w, h) = (size.width as i64, size.height as i64);
    let reach = radius + thickness;
    let x0 = ((cx - reach).floor() as i64).max(0);
    let x1 = ((cx + reach).ceil() as i64).min(w - 1);
    let y0 = ((cy - reach).floor() as i64).max(0);
    let y1 = ((cy + reach).ceil() as i64).min(h - 1);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
            let coverage = (thickness / 2.0 + 0.5 - (d - radius).abs()).clamp(0.0, 1.0);
            let a = coverage * alpha;
            if a <= 0.0 {
                continue;
            }
            let i = (y * w + x) as usize * 4;
            for c in 0..3 {
                let v = frame[i + c] as f64;
                frame[i + c] = (v + (color[c] as f64 - v) * a).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::CursorSample;

    fn pixel(frame: &[u8], size: Size, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * size.width + x) * 4) as usize;
        [frame[i], frame[i + 1], frame[i + 2]]
    }

    #[test]
    fn ring_appears_after_click_and_fades() {
        let s = |t, pressed| CursorSample {
            t,
            x: 100.0,
            y: 100.0,
            pressed,
        };
        let track = CursorTrack {
            samples: vec![s(0.0, false), s(1.0, true), s(1.1, false)],
        };
        let rings = ClickRings::new(&track);
        let size = Size::new(200, 200);
        let view = Rect::full(size);
        let draw = |t| {
            let mut f = vec![0u8; 200 * 200 * 4];
            rings.draw(&mut f, size, view, t);
            f
        };

        // Before the click and after the ring has faded: untouched.
        assert!(draw(0.9).iter().all(|&v| v == 0));
        assert!(draw(1.6).iter().all(|&v| v == 0));
        // Right at the click: ring of radius ~5 (10 * 200/1280 clamped to 0.5).
        let f = draw(1.0);
        assert_eq!(pixel(&f, size, 100, 100), [0, 0, 0], "centre stays clear");
        assert_ne!(pixel(&f, size, 105, 100), [0, 0, 0]);
    }

    #[test]
    fn outline_darkens_light_background() {
        let size = Size::new(100, 100);
        let mut f = vec![255u8; 100 * 100 * 4];
        draw_ring(&mut f, size, 50.0, 50.0, 20.0, 7.0, [0, 0, 0], 1.0);
        draw_ring(&mut f, size, 50.0, 50.0, 20.0, 3.0, [255, 196, 0], 1.0);
        assert_eq!(pixel(&f, size, 70, 50), [255, 196, 0], "ring centre");
        assert!(pixel(&f, size, 72, 50)[0] < 100, "outline");
        assert_eq!(pixel(&f, size, 80, 50), [255, 255, 255], "outside");
    }

    #[test]
    fn ring_follows_zoomed_view() {
        let s = |t, pressed| CursorSample {
            t,
            x: 150.0,
            y: 150.0,
            pressed,
        };
        let track = CursorTrack {
            samples: vec![s(0.0, false), s(1.0, true)],
        };
        let rings = ClickRings::new(&track);
        let size = Size::new(200, 200);
        // 2x zoom on the bottom-right quadrant: click maps to (100, 100).
        let view = Rect {
            x: 100.0,
            y: 100.0,
            width: 100.0,
            height: 100.0,
        };
        let mut f = vec![0u8; 200 * 200 * 4];
        rings.draw(&mut f, size, view, 1.0);
        assert_ne!(pixel(&f, size, 105, 100), [0, 0, 0]);
        assert_eq!(pixel(&f, size, 155, 150), [0, 0, 0]);
    }
}
