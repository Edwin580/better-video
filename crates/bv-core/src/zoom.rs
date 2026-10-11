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

    fn step(&mut self, target: f64, omega: f64, dt: f64) {
        let c1 = self.x - target;
        let c2 = self.v + omega * c1;
        let e = (-omega * dt).exp();
        self.x = target + (c1 + c2 * dt) * e;
        self.v = (c2 - omega * (c1 + c2 * dt)) * e;
    }
}

/// Produces one viewport per output frame. Stateful (it eases toward the
/// target), so frames must be requested in order via [`ZoomPlanner::next`].
pub struct ZoomPlanner<'a> {
    cfg: ZoomConfig,
    track: &'a CursorTrack,
    bounds: Size,
    /// Sorted, non-overlapping spans during which the camera is zoomed in.
    active: Vec<Span>,
    /// Where the camera wants to be centred.
    target: (f64, f64),
    was_active: bool,
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
        let active = active_spans(&cfg, track, bounds);
        let start_zoom = if cfg.trigger == ZoomTrigger::Follow {
            cfg.level
        } else {
            1.0
        };
        Self {
            cfg,
            track,
            bounds,
            active,
            target: (cx, cy),
            was_active: false,
            log_zoom: Spring::new(start_zoom.max(1.0).ln()),
            cx: Spring::new(cx),
            cy: Spring::new(cy),
            last_t: None,
        }
    }

    fn span_at(&self, t: f64) -> Option<&Span> {
        let i = self.active.partition_point(|s| s.end < t);
        self.active.get(i).filter(|s| s.start <= t)
    }

    /// Viewport for the frame at time `t` (seconds). `t` must not decrease.
    pub fn next(&mut self, t: f64) -> Rect {
        let cursor = self.track.position_at(t).unwrap_or(self.target);
        let span = self.span_at(t).copied();
        let active = span.is_some();
        let level = if active { self.cfg.level.max(1.0) } else { 1.0 };

        match span {
            // Zooming in: aim at what's about to be clicked / looked at,
            // even though the cursor may still be on its way there.
            Some(s) if !self.was_active => self.target = s.focus.unwrap_or(cursor),
            Some(s) if t < s.event => {}
            Some(_) => self.target = follow(self.target, cursor, self.bounds, level),
            None => {}
        }
        self.was_active = active;

        let dt = self.last_t.map_or(0.0, |last| (t - last).max(0.0));
        self.last_t = Some(t);
        // A critically damped spring settles (to ~1%) in about 6.6 / omega.
        let omega = 6.6 / self.cfg.smoothing_secs.max(0.05);
        self.log_zoom.step(level.ln(), omega, dt);
        // Pan a little faster than zoom so the subject stays in frame.
        self.cx.step(self.target.0, omega * 1.25, dt);
        self.cy.step(self.target.1, omega * 1.25, dt);

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

/// A stretch of time the camera is zoomed in.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Span {
    start: f64,
    end: f64,
    /// When the click / rest happens (after `start`, which leads it).
    event: f64,
    /// Where to aim: the click or the resting point.
    focus: Option<(f64, f64)>,
}

/// When the camera should be zoomed in, merged and sorted. Spans start a
/// little early (export knows the future), so the zoom is already under
/// way when the click or pause happens.
fn active_spans(cfg: &ZoomConfig, track: &CursorTrack, bounds: Size) -> Vec<Span> {
    let lead = cfg.smoothing_secs * 0.5;
    let (click, hover) = match cfg.trigger {
        ZoomTrigger::Follow => {
            return vec![Span {
                start: f64::NEG_INFINITY,
                end: f64::INFINITY,
                event: f64::NEG_INFINITY,
                focus: None,
            }]
        }
        ZoomTrigger::Click => (true, false),
        ZoomTrigger::Hover => (false, true),
        ZoomTrigger::ClickOrHover => (true, true),
    };
    let span = |event: f64, end: f64| Span {
        start: event - lead,
        end: end + cfg.hold_secs,
        event,
        focus: track.position_at(event),
    };
    let mut spans: Vec<Span> = Vec::new();
    if click {
        spans.extend(track.clicks().map(|c| span(c, c)));
    }
    if hover {
        // Resting = staying within a small radius, relative to the screen.
        let radius = (bounds.width.max(bounds.height) as f64 / 100.0).max(8.0);
        spans.extend(
            rests(track, radius, cfg.dwell_secs)
                .into_iter()
                .map(|(s, e)| span(s, e)),
        );
    }
    spans.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut merged: Vec<Span> = Vec::new();
    for s in spans {
        match merged.last_mut() {
            Some(last) if s.start <= last.end + MERGE_GAP_SECS => last.end = last.end.max(s.end),
            _ => merged.push(s),
        }
    }
    merged
}

/// Spans of at least `min_secs` during which the cursor stays within
/// `radius` of where the span started.
fn rests(track: &CursorTrack, radius: f64, min_secs: f64) -> Vec<(f64, f64)> {
    let s = &track.samples;
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let mut j = i;
        while j + 1 < s.len() && (s[j + 1].x - s[i].x).hypot(s[j + 1].y - s[i].y) <= radius {
            j += 1;
        }
        if s[j].t - s[i].t >= min_secs {
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
        let spans = active_spans(&ZoomConfig::default(), &tr, Size::new(1000, 600));
        assert_eq!(spans.len(), 1, "{spans:?}");
    }
}
