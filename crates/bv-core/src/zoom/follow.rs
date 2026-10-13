//! The camera path for `--zoom-on follow`: a view that trails the pointer
//! with a dead zone, so small movements don't move the picture.
//!
//! Ported from OpenScreen (https://github.com/getopenscreen/openscreen),
//! crates/compositor/src/cursor.rs (`follow_step`, `follow_in_view` /
//! `view_states`) and src/lib/zoomMath/constants.ts
//! (`AUTO_FOLLOW_PARAMS`), MIT License,
//! Copyright (c) 2025 Siddharth Vaddem,
//! Copyright (c) 2025-2026 OpenScreen contributors.
//! See THIRD-PARTY-NOTICES.md. Changes: f64; the whole clip is one
//! region, replayed once up front instead of cached; and the view snaps onto its
//! target once within a quarter of a source pixel, so a parked pointer
//! gives bit-identical frames (an exponential approach never arrives).

use crate::{cursor::CursorTrack, geometry::Size};

// Auto-follow smoothing: a per-frame factor that grows with the distance
// to the target (far = catch up fast, near = decelerate), made frame-rate
// independent against a 40 fps reference frame.
const AUTO_FOLLOW_MIN_FACTOR: f64 = 0.1;
const AUTO_FOLLOW_MAX_FACTOR: f64 = 0.25;
const AUTO_FOLLOW_RAMP_DISTANCE: f64 = 0.15;
const AUTO_FOLLOW_REFERENCE_MS: f64 = 1000.0 / 40.0;

/// Share of the zoomed view, per axis, where the pointer moves without
/// the view following it: its central half. Beyond it, the view catches
/// up just enough to bring the pointer back to the edge of that zone.
const FOLLOW_DEAD_ZONE: f64 = 0.5;

/// Snap distance in source pixels.
const SETTLE_PX: f64 = 0.25;

/// One smoothing step: `from` moves toward `to` by a factor that grows
/// with the distance, corrected for `dt_ms`.
fn follow_step(from: (f64, f64), to: (f64, f64), dt_ms: f64) -> (f64, f64) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let distance = (dx * dx + dy * dy).sqrt();
    let ramp = (distance / AUTO_FOLLOW_RAMP_DISTANCE).min(1.0);
    let base = AUTO_FOLLOW_MIN_FACTOR + (AUTO_FOLLOW_MAX_FACTOR - AUTO_FOLLOW_MIN_FACTOR) * ramp;
    let factor = 1.0 - (1.0 - base).powf(dt_ms / AUTO_FOLLOW_REFERENCE_MS);
    (from.0 + dx * factor, from.1 + dy * factor)
}

/// The view centre of a `scale` zoom over the whole track, in 0..1 of the
/// frame, as `(t, x, y)` states to interpolate between.
pub struct FollowPath {
    states: Vec<(f64, f64, f64)>,
}

impl FollowPath {
    pub fn new(track: &CursorTrack, bounds: Size, scale: f64) -> Self {
        let (w, h) = (bounds.width.max(1) as f64, bounds.height.max(1) as f64);
        let samples: Vec<(f64, f64, f64)> = track
            .samples
            .iter()
            .map(|s| (s.t, s.x / w, s.y / h))
            .collect();
        let half = 0.5 / scale.max(1.0);
        let dead = half * FOLLOW_DEAD_ZONE;
        let reach = |v: f64| v.clamp(half, 1.0 - half);
        // The target: the point that brings the pointer back to the edge of
        // the zone, or the view itself while the pointer is inside it.
        let pull = |cursor: f64, view: f64| cursor - (cursor - view).clamp(-dead, dead);
        let (eps_x, eps_y) = (SETTLE_PX / w, SETTLE_PX / h);
        let step = |view: (f64, f64), x: f64, y: f64, dt: f64| {
            let to = (pull(x, view.0), pull(y, view.1));
            let (nx, ny) = follow_step(view, to, dt * 1000.0);
            let nx = if (nx - to.0).abs() < eps_x { to.0 } else { nx };
            let ny = if (ny - to.1).abs() < eps_y { to.1 } else { ny };
            (reach(nx), reach(ny))
        };

        let Some(&(t0, x0, y0)) = samples.first() else {
            return Self { states: Vec::new() };
        };
        let mut view = (reach(x0), reach(y0));
        let mut states = vec![(t0, view.0, view.1)];
        for &(ts, x, y) in &samples[1..] {
            let at = states[states.len() - 1].0;
            view = step(view, x, y, (ts - at).max(0.0));
            states.push((ts, view.0, view.1));
        }
        // After the last sample the view keeps catching up with the parked
        // pointer instead of freezing mid-way; replay that at 120 Hz until
        // it has snapped.
        if let Some(&(t_last, x, y)) = samples.last() {
            let mut t = t_last;
            for _ in 0..1200 {
                let next = step(view, x, y, 1.0 / 120.0);
                if next == view {
                    break;
                }
                t += 1.0 / 120.0;
                view = next;
                states.push((t, view.0, view.1));
            }
        }
        Self { states }
    }

    /// View centre at `t`, in 0..1 of the frame.
    pub fn at(&self, t: f64) -> Option<(f64, f64)> {
        let s = &self.states;
        let first = s.first()?;
        if t <= first.0 {
            return Some((first.1, first.2));
        }
        let i = s.partition_point(|p| p.0 <= t);
        if i >= s.len() {
            let last = s[s.len() - 1];
            return Some((last.1, last.2));
        }
        let (a, b) = (s[i - 1], s[i]);
        let f = if b.0 > a.0 {
            (t - a.0) / (b.0 - a.0)
        } else {
            1.0
        };
        Some((a.1 + (b.1 - a.1) * f, a.2 + (b.2 - a.2) * f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::CursorSample;

    fn track(points: &[(f64, f64, f64)]) -> CursorTrack {
        CursorTrack {
            samples: points
                .iter()
                .map(|&(t, x, y)| CursorSample {
                    t,
                    x,
                    y,
                    pressed: false,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn holds_still_inside_the_dead_zone() {
        // Wiggling around the centre of a 2x view.
        let pts: Vec<_> = (0..100)
            .map(|i| {
                (
                    i as f64 * 0.02,
                    500.0 + if i % 2 == 0 { 40.0 } else { -40.0 },
                    300.0,
                )
            })
            .collect();
        let p = FollowPath::new(&track(&pts), Size::new(1000, 600), 2.0);
        let (a, b) = (p.at(0.5).unwrap(), p.at(1.9).unwrap());
        assert!((a.0 - b.0).abs() < 1e-3, "{a:?} {b:?}");
    }

    #[test]
    fn catches_up_and_settles_exactly() {
        let pts = [
            (0.0, 100.0, 100.0),
            (0.5, 950.0, 550.0),
            (1.0, 950.0, 550.0),
        ];
        let bounds = Size::new(1000, 600);
        let p = FollowPath::new(&track(&pts), bounds, 2.0);
        let end = p.at(10.0).unwrap();
        // Reaches the corner as far as a half-size view can go.
        assert!(
            (end.0 - 0.75).abs() < 1e-9 && (end.1 - 0.75).abs() < 1e-9,
            "{end:?}"
        );
        assert_eq!(p.at(9.0), p.at(10.0));
    }
}
