//! Cursor-following "camera" that decides which part of the screen each
//! exported frame shows.
//!
//! The camera is driven by critically damped springs (on log-zoom and on
//! the centre), so it accelerates and settles smoothly with no overshoot
//! and no velocity jump when a zoom starts. While zoomed it only pans when
//! the cursor nears the edge of the view, so small movements don't shake
//! the picture.

use serde::{Deserialize, Serialize};

use crate::{cursor::CursorTrack, geometry::Rect, geometry::Size};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ZoomTrigger {
    /// Zoom in around each click, ease back out after `hold_secs`.
    #[default]
    Click,
    /// Zoom in where the cursor comes to rest for `dwell_secs`.
    Hover,
    /// Either of the above.
    ClickOrHover,
    /// Stay zoomed in and follow the cursor for the whole clip.
    #[serde(alias = "always")]
    Follow,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ZoomConfig {
    /// Magnification when zoomed in (2.0 = show a quarter of the screen).
    pub level: f64,
    pub trigger: ZoomTrigger,
    /// How long to stay zoomed after the last click / after the cursor
    /// moves on.
    pub hold_secs: f64,
    /// Roughly how long the camera takes to settle on a new target.
    /// Larger is smoother and slower.
    pub smoothing_secs: f64,
    /// How long the cursor must rest to count as hovering.
    pub dwell_secs: f64,
}

impl Default for ZoomConfig {
    fn default() -> Self {
        Self {
            level: 2.0,
            trigger: ZoomTrigger::Click,
            hold_secs: 1.5,
            smoothing_secs: 0.8,
            dwell_secs: 0.6,
        }
    }
}

/// Zoom-ins closer together than this merge into one, so the camera
/// doesn't pump out and back in.
const MERGE_GAP_SECS: f64 = 0.8;
/// While zoomed, the cursor may roam this fraction of the view (centred)
/// before the camera pans.
const DEAD_ZONE: f64 = 0.5;

/// Critically damped spring, advanced with its exact solution so the
/// motion doesn't depend on the frame rate.
#[derive(Debug, Clone, Copy)]
struct Spring {
    x: f64,
    v: f64,
}

impl Spring {
    fn new(x: f64) -> Self {
        Self { x, v: 0.0 }
    }

    /// Advance by `dt` toward `target`. Within `eps` of it and nearly
    /// still, it snaps exactly onto it: an exponential approach never
    /// quite arrives, and sub-pixel drift would change every frame (a
    /// faint shimmer, and a GIF that can't skip unchanged frames).
    fn step(&mut self, target: f64, omega: f64, dt: f64, eps: f64) {
        let c1 = self.x - target;
        let c2 = self.v + omega * c1;
        let e = (-omega * dt).exp();
        self.x = target + (c1 + c2 * dt) * e;
        self.v = (c2 - omega * (c1 + c2 * dt)) * e;
        if (self.x - target).abs() < eps && self.v.abs() < eps * omega {
            self.x = target;
            self.v = 0.0;
        }
    }
}

/// Snap thresholds: 0.1% of zoom, a quarter of a source pixel.
const SETTLE_LOG_ZOOM: f64 = 1e-3;
const SETTLE_PX: f64 = 0.25;

/// Produces one viewport per output frame. Stateful (it eases toward the
/// target), so frames must be requested in order via [`ZoomPlanner::next`].
pub struct ZoomPlanner<'a> {
    cfg: ZoomConfig,
    track: &'a CursorTrack,
    bounds: Size,
    plan: Plan,
    /// Next aim in `plan.aims` not yet reached.
    next_aim: usize,
    /// Where the camera wants to be centred.
    target: (f64, f64),
    /// Until the click / rest being aimed at happens, hold the target
    /// instead of following the cursor (which may still be on its way).
    hold_until: f64,
    log_zoom: Spring,
    cx: Spring,
    cy: Spring,
    last_t: Option<f64>,
}

impl<'a> ZoomPlanner<'a> {
    pub fn new(cfg: ZoomConfig, track: &'a CursorTrack, bounds: Size) -> Self {
        let (cx, cy) = track
            .position_at(0.0)
            .unwrap_or((bounds.width as f64 / 2.0, bounds.height as f64 / 2.0));
        let plan = plan(&cfg, track, bounds);
        let start_zoom = if cfg.trigger == ZoomTrigger::Follow {
            cfg.level
        } else {
            1.0
        };
        Self {
            cfg,
            track,
            bounds,
            plan,
            next_aim: 0,
            target: (cx, cy),
            hold_until: f64::NEG_INFINITY,
            log_zoom: Spring::new(start_zoom.max(1.0).ln()),
            cx: Spring::new(cx),
            cy: Spring::new(cy),
            last_t: None,
        }
    }

    fn zoomed_at(&self, t: f64) -> bool {
        let spans = &self.plan.spans;
        let i = spans.partition_point(|s| s.1 < t);
        spans.get(i).is_some_and(|s| s.0 <= t)
    }

    /// Viewport for the frame at time `t` (seconds). `t` must not decrease.
    pub fn next(&mut self, t: f64) -> Rect {
        let cursor = self.track.position_at(t).unwrap_or(self.target);
        let active = self.zoomed_at(t);
        let level = if active { self.cfg.level.max(1.0) } else { 1.0 };

        // Each click / rest re-aims the camera when its lead time arrives,
        // even if it's already zoomed in from an earlier one.
        while let Some(aim) = self.plan.aims.get(self.next_aim) {
            if aim.from > t {
                break;
            }
            self.target = aim.point;
            self.hold_until = aim.event;
            self.next_aim += 1;
        }
        if active && t >= self.hold_until {
            self.target = follow(self.target, cursor, self.bounds, level);
        }

        let dt = self.last_t.map_or(0.0, |last| (t - last).max(0.0));
        self.last_t = Some(t);
        // A critically damped spring settles (to ~1%) in about 6.6 / omega.
        let omega = 6.6 / self.cfg.smoothing_secs.max(0.05);
        self.log_zoom.step(level.ln(), omega, dt, SETTLE_LOG_ZOOM);
        // Pan a little faster than zoom so the subject stays in frame.
        self.cx.step(self.target.0, omega * 1.25, dt, SETTLE_PX);
        self.cy.step(self.target.1, omega * 1.25, dt, SETTLE_PX);

        Rect::zoomed(self.bounds, self.log_zoom.x.exp(), self.cx.x, self.cy.x)
    }
}

/// Move `target` just enough that `cursor` stays inside the dead zone of a
/// view of `level` magnification centred on it.
fn follow(target: (f64, f64), cursor: (f64, f64), bounds: Size, level: f64) -> (f64, f64) {
    let half_w = bounds.width as f64 / level / 2.0 * DEAD_ZONE;
    let half_h = bounds.height as f64 / level / 2.0 * DEAD_ZONE;
    let axis = |t: f64, c: f64, half: f64| {
        if c > t + half {
            c - half
        } else if c < t - half {
            c + half
        } else {
            t
        }
    };
    (
        axis(target.0, cursor.0, half_w),
        axis(target.1, cursor.1, half_h),
    )
}

/// A click or rest the camera should turn to.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Aim {
    /// When to start turning (a little before `event`).
    from: f64,
    /// When the click / rest happens.
    event: f64,
    point: (f64, f64),
}

#[derive(Debug, Clone, PartialEq, Default)]
struct Plan {
    /// Sorted, non-overlapping `(start, end)` times the camera is zoomed in.
    spans: Vec<(f64, f64)>,
    /// Sorted by `from`.
    aims: Vec<Aim>,
}

/// When the camera is zoomed in and where it aims. Zooms start a little
/// early (export knows the future), so they're under way when the click
/// or pause happens; zooms close together merge into one span, but each
/// keeps its own aim.
fn plan(cfg: &ZoomConfig, track: &CursorTrack, bounds: Size) -> Plan {
    let lead = cfg.smoothing_secs * 0.5;
    let (click, hover) = match cfg.trigger {
        ZoomTrigger::Follow => {
            return Plan {
                spans: vec![(f64::NEG_INFINITY, f64::INFINITY)],
                aims: Vec::new(),
            }
        }
        ZoomTrigger::Click => (true, false),
        ZoomTrigger::Hover => (false, true),
        ZoomTrigger::ClickOrHover => (true, true),
    };
    // (event, end of the thing that triggered it)
    let mut events: Vec<(f64, f64)> = Vec::new();
    if click {
        events.extend(track.clicks().map(|c| (c, c)));
    }
    if hover {
        // Resting = staying within a small radius, relative to the screen.
        let radius = (bounds.width.max(bounds.height) as f64 / 100.0).max(8.0);
        events.extend(rests(track, radius, cfg.dwell_secs));
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut plan = Plan::default();
    for &(event, end) in &events {
        let span = (event - lead, end + cfg.hold_secs);
        match plan.spans.last_mut() {
            Some(last) if span.0 <= last.1 + MERGE_GAP_SECS => last.1 = last.1.max(span.1),
            _ => plan.spans.push(span),
        }
        if let Some(point) = track.position_at(event) {
            plan.aims.push(Aim {
                from: event - lead,
                event,
                point,
            });
        }
    }
    plan
}

/// Spans of at least `min_secs` during which the cursor stays within
/// `radius` of where the span started. A rest from the very first sample
/// is skipped: that's just where the mouse was when recording began, not
/// something the user moved to.
fn rests(track: &CursorTrack, radius: f64, min_secs: f64) -> Vec<(f64, f64)> {
    let s = &track.samples;
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let mut j = i;
        while j + 1 < s.len() && (s[j + 1].x - s[i].x).hypot(s[j + 1].y - s[i].y) <= radius {
            j += 1;
        }
        if i > 0 && s[j].t - s[i].t >= min_secs {
            out.push((s[i].t, s[j].t));
        }
        i = j + 1;
    }
    out
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

    fn run(planner: &mut ZoomPlanner, until: f64) -> Rect {
        let mut r = planner.next(0.0);
        let mut t = 0.0;
        while t < until {
            t += 1.0 / 30.0;
            r = planner.next(t);
        }
        r
    }

    #[test]
    fn no_clicks_means_no_zoom() {
        let tr = track(&[(0.0, 100.0, 100.0, false), (5.0, 900.0, 500.0, false)]);
        let bounds = Size::new(1000, 600);
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, bounds);
        assert_eq!(run(&mut p, 3.0), Rect::full(bounds));
    }

    #[test]
    fn click_zooms_in_then_out() {
        let tr = track(&[
            (0.0, 800.0, 400.0, false),
            (1.0, 800.0, 400.0, true),
            (1.1, 800.0, 400.0, false),
            (10.0, 800.0, 400.0, false),
        ]);
        let bounds = Size::new(1000, 600);
        let cfg = ZoomConfig::default();
        let mut p = ZoomPlanner::new(cfg, &tr, bounds);
        let zoomed = run(&mut p, 2.3);
        assert!((zoomed.width - 500.0).abs() < 10.0, "{zoomed:?}");
        // Camera hugs the cursor but stays inside the screen.
        assert!(zoomed.x + zoomed.width <= 1000.0 && zoomed.x > 400.0);

        let mut p = ZoomPlanner::new(cfg, &tr, bounds);
        let out = run(&mut p, 6.0);
        assert!((out.width - 1000.0).abs() < 1.0, "{out:?}");
    }

    #[test]
    fn zoom_starts_and_ends_gently() {
        // Click at 2 s; sample zoom every frame and look at the steps.
        let tr = track(&[
            (0.0, 500.0, 300.0, false),
            (2.0, 500.0, 300.0, true),
            (2.1, 500.0, 300.0, false),
            (9.0, 500.0, 300.0, false),
        ]);
        let bounds = Size::new(1000, 600);
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, bounds);
        let widths: Vec<f64> = (0..270).map(|i| p.next(i as f64 / 30.0).width).collect();
        let steps: Vec<f64> = widths.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        let biggest = steps.iter().cloned().fold(0.0, f64::max);
        // Spring motion ramps up: the first moving frame is a small fraction
        // of the peak speed (an exponential ease starts at its peak).
        let first = steps.iter().position(|&s| s > 1e-6).unwrap();
        assert!(
            steps[first] < biggest * 0.5,
            "{} vs {biggest}",
            steps[first]
        );
        // And it never overshoots past full zoom.
        assert!(widths.iter().all(|&w| w >= 500.0 - 1e-6), "overshoot");
    }

    #[test]
    fn hover_zooms_where_the_cursor_rests() {
        // Moving, then resting at (200, 150) for a second, then moving on.
        let mut s = Vec::new();
        for i in 0..=20 {
            s.push((
                i as f64 * 0.05,
                900.0 - i as f64 * 35.0,
                500.0 - i as f64 * 17.5,
                false,
            ));
        }
        for i in 1..=20 {
            s.push((1.0 + i as f64 * 0.05, 200.0, 150.0, false));
        }
        s.push((4.0, 900.0, 500.0, false));
        let tr = track(&s);
        let bounds = Size::new(1000, 600);
        let cfg = ZoomConfig {
            trigger: ZoomTrigger::Hover,
            ..Default::default()
        };
        let mut p = ZoomPlanner::new(cfg, &tr, bounds);
        let r = run(&mut p, 2.2);
        assert!(r.width < 560.0, "zoomed in: {r:?}");
        assert!(
            r.x < 200.0 && r.x + r.width > 200.0,
            "around the rest point: {r:?}"
        );

        let click_only = ZoomConfig::default();
        let mut p = ZoomPlanner::new(click_only, &tr, bounds);
        assert_eq!(run(&mut p, 2.2), Rect::full(bounds));
    }

    #[test]
    fn small_movements_dont_pan() {
        // Zoomed in, the cursor wiggles near the centre.
        let mut s = vec![(0.0, 500.0, 300.0, true), (0.1, 500.0, 300.0, false)];
        for i in 0..60 {
            let d = if i % 2 == 0 { 30.0 } else { -30.0 };
            s.push((0.2 + i as f64 * 0.05, 500.0 + d, 300.0, false));
        }
        let tr = track(&s);
        let cfg = ZoomConfig {
            trigger: ZoomTrigger::Follow,
            ..Default::default()
        };
        let mut p = ZoomPlanner::new(cfg, &tr, Size::new(1000, 600));
        let a = run(&mut p, 1.5);
        let b = run(&mut p, 3.0);
        assert!((a.x - b.x).abs() < 1.0, "{a:?} {b:?}");
    }

    #[test]
    fn follow_mode_tracks_the_cursor() {
        let tr = track(&[(0.0, 0.0, 0.0, false), (1.0, 1000.0, 600.0, false)]);
        let cfg = ZoomConfig {
            trigger: ZoomTrigger::Follow,
            ..Default::default()
        };
        let mut p = ZoomPlanner::new(cfg, &tr, Size::new(1000, 600));
        let r = run(&mut p, 4.0);
        assert!((r.width - 500.0).abs() < 1e-6);
        assert!(
            (r.x - 500.0).abs() < 1.0 && (r.y - 300.0).abs() < 1.0,
            "{r:?}"
        );
    }

    #[test]
    fn nearby_zooms_merge() {
        let tr = track(&[
            (0.0, 0.0, 0.0, false),
            (1.0, 0.0, 0.0, true),
            (1.1, 0.0, 0.0, false),
            (3.0, 0.0, 0.0, true),
            (3.1, 0.0, 0.0, false),
        ]);
        let p = plan(&ZoomConfig::default(), &tr, Size::new(1000, 600));
        assert_eq!(p.spans.len(), 1, "{p:?}");
        assert_eq!(p.aims.len(), 2, "each click still aims: {p:?}");
    }

    #[test]
    fn settles_exactly_so_still_frames_are_identical() {
        // Click, then hold still: once settled, consecutive views must be
        // bit-identical (no sub-pixel drift).
        let tr = track(&[
            (0.0, 600.0, 300.0, false),
            (0.5, 600.0, 300.0, true),
            (0.6, 600.0, 300.0, false),
            (8.0, 600.0, 300.0, false),
        ]);
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, Size::new(1000, 600));
        let views: Vec<Rect> = (0..60).map(|i| p.next(i as f64 / 30.0)).collect();
        assert_eq!(views[57], views[58]);
        assert_eq!(views[58], views[59]);
        assert!((views[59].width - 500.0).abs() < 1e-9, "{:?}", views[59]);
    }

    #[test]
    fn resting_where_recording_began_is_not_hover() {
        // The cursor sits still for 2 s at the start, then moves away.
        let tr = track(&[
            (0.0, 900.0, 500.0, false),
            (2.0, 900.0, 500.0, false),
            (2.5, 100.0, 100.0, false),
            (2.6, 400.0, 300.0, false),
        ]);
        let cfg = ZoomConfig {
            trigger: ZoomTrigger::Hover,
            ..Default::default()
        };
        assert!(plan(&cfg, &tr, Size::new(1000, 600)).spans.is_empty());
    }

    #[test]
    fn a_second_click_while_zoomed_re_aims() {
        // Click top-left, then (still zoomed) bottom-right.
        let tr = track(&[
            (0.0, 150.0, 100.0, false),
            (1.0, 150.0, 100.0, true),
            (1.1, 150.0, 100.0, false),
            (1.8, 850.0, 500.0, false),
            (2.0, 850.0, 500.0, true),
            (2.1, 850.0, 500.0, false),
            (6.0, 850.0, 500.0, false),
        ]);
        let bounds = Size::new(1000, 600);
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, bounds);
        let first = run(&mut p, 1.3);
        assert!(first.x < 60.0, "on the first click: {first:?}");
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, bounds);
        let second = run(&mut p, 3.2);
        assert!(
            second.x + second.width > 900.0,
            "moved to the second: {second:?}"
        );
    }
}
