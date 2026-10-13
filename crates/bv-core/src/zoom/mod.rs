//! The "camera" that decides which part of the screen each exported frame
//! shows.
//!
//! Zooming is planned like an editor would, from the whole take at once
//! (export knows the future): clicks (or rests) close in time and space
//! share one zoom, just deep enough to frame them with a margin; zooms
//! never come straight back after leaving; and the clip opens and ends on
//! the whole screen ([`shots`]). Each zoom then eases in and out over a
//! fixed, depth-dependent time with a spring curve that starts and lands
//! at rest ([`envelope`]), and holds perfectly still in between, so still
//! frames are bit-identical. `follow` mode instead stays zoomed and trails
//! the pointer with a dead zone ([`follow`]).
//!
//! The planning, easing and follow are ported from OpenScreen; see each
//! module. The focus easing in [`ZoomPlanner::view_at`] is ported from
//! OpenScreen (https://github.com/getopenscreen/openscreen),
//! crates/compositor/src/regions.rs (`zoom_state_in`), MIT License,
//! Copyright (c) 2025 Siddharth Vaddem,
//! Copyright (c) 2025-2026 OpenScreen contributors.
//! See THIRD-PARTY-NOTICES.md.

mod envelope;
mod follow;
mod shots;

use serde::{Deserialize, Serialize};

use crate::{cursor::CursorTrack, geometry::Rect, geometry::Size};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ZoomTrigger {
    /// Zoom in around clicks, ease back out `hold_secs` after the last.
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
    /// The deepest magnification a zoom may use (1.8 = show 56% of the
    /// screen). Click and hover zooms pick the deepest of 1.25, 1.5 and
    /// 1.8 (and this level, if deeper) that still frames everything they
    /// cover; `follow` uses exactly this.
    pub level: f64,
    pub trigger: ZoomTrigger,
    /// How long to stay zoomed after the last click / after the cursor
    /// moves on.
    pub hold_secs: f64,
    /// Pace of camera moves: at the default 0.8 a zoom to 1.25x takes
    /// 0.7 s and to 1.8x 0.9 s; moves scale in proportion.
    pub smoothing_secs: f64,
    /// How long the cursor must rest to count as hovering.
    pub dwell_secs: f64,
}

/// Default for [`ZoomConfig::level`].
pub const DEFAULT_LEVEL: f64 = 1.8;
/// Default for [`ZoomConfig::smoothing_secs`]: OpenScreen's own pace.
pub const DEFAULT_SMOOTHING: f64 = 0.8;

impl Default for ZoomConfig {
    fn default() -> Self {
        Self {
            level: DEFAULT_LEVEL,
            trigger: ZoomTrigger::Click,
            hold_secs: 1.5,
            smoothing_secs: DEFAULT_SMOOTHING,
            dwell_secs: 0.6,
        }
    }
}

impl ZoomConfig {
    /// Seconds a move to (or from) `scale` takes.
    fn ramp(&self, scale: f64) -> f64 {
        envelope::zoom_transition_s(scale) * self.smoothing_secs.max(0.05) / DEFAULT_SMOOTHING
    }

    /// The depths a click or hover zoom may use, shallowest first.
    fn depths(&self) -> Vec<f64> {
        let level = self.level;
        let mut d: Vec<f64> = shots::SHOT_DEPTHS
            .into_iter()
            .filter(|&s| s <= level)
            .collect();
        if level > 1.0 && d.last().is_none_or(|&s| s < level) {
            d.push(level);
        }
        d
    }
}

/// One planned zoom, ready to render.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Zoom {
    /// Fully zoomed in over `start..=end`; the moves lie outside.
    start: f64,
    end: f64,
    scale: f64,
    /// Seconds each move takes.
    ramp: f64,
    /// What it centres on at full zoom, in 0..1 of the frame, already
    /// within what a view of this scale can reach.
    focus: (f64, f64),
}

/// Produces one viewport per output frame. Each viewport is a pure
/// function of `t`, so frames may be requested in any order.
pub struct ZoomPlanner {
    cfg: ZoomConfig,
    bounds: Size,
    zooms: Vec<Zoom>,
    follow: Option<follow::FollowPath>,
}

impl ZoomPlanner {
    pub fn new(cfg: ZoomConfig, track: &CursorTrack, bounds: Size) -> Self {
        let (zooms, follow) = if cfg.trigger == ZoomTrigger::Follow {
            let level = cfg.level.max(1.0);
            (
                Vec::new(),
                Some(follow::FollowPath::new(track, bounds, level)),
            )
        } else {
            (plan(&cfg, track, bounds), None)
        };
        Self {
            cfg,
            bounds,
            zooms,
            follow,
        }
    }

    /// Viewport for the frame at time `t` (seconds).
    pub fn next(&mut self, t: f64) -> Rect {
        self.view_at(t)
    }

    /// Viewport at time `t` (seconds).
    pub fn view_at(&self, t: f64) -> Rect {
        let (w, h) = (self.bounds.width as f64, self.bounds.height as f64);
        if let Some(path) = &self.follow {
            let level = self.cfg.level.max(1.0);
            let (x, y) = path.at(t).unwrap_or((0.5, 0.5));
            return Rect::zoomed(self.bounds, level, x * w, y * h);
        }
        // The strongest zoom at `t`, the later one on a tie (the planner
        // keeps them apart, so there is at most one).
        let mut best: Option<(&Zoom, f64)> = None;
        for z in &self.zooms {
            let s = envelope::strength(z.start, z.end, z.ramp, t);
            if s > 0.0 && best.is_none_or(|(_, b)| s >= b) {
                best = Some((z, s));
            }
        }
        let Some((z, strength)) = best else {
            return Rect::full(self.bounds);
        };
        let scale = envelope::scale_lerp(1.0, z.scale, strength);
        // Bring the focus point to its place on screen linearly with the
        // strength, as OpenScreen does (regions.rs, `zoom_state_in`):
        // centring the crop on `lerp(0.5, focus, strength)` would hold the
        // point back mid-move and then catch up, which reads as a drift.
        let ease = |f: f64| f - (f - 0.5) * (1.0 - strength) / scale;
        Rect::zoomed(self.bounds, scale, ease(z.focus.0) * w, ease(z.focus.1) * h)
    }
}

/// The zooms for a take: its clicks and/or rests, planned into shots.
fn plan(cfg: &ZoomConfig, track: &CursorTrack, bounds: Size) -> Vec<Zoom> {
    let (click, hover) = match cfg.trigger {
        ZoomTrigger::Follow => return Vec::new(),
        ZoomTrigger::Click => (true, false),
        ZoomTrigger::Hover => (false, true),
        ZoomTrigger::ClickOrHover => (true, true),
    };
    let (w, h) = (bounds.width.max(1) as f64, bounds.height.max(1) as f64);
    let at = |t: f64| track.position_at(t).map(|(x, y)| (x / w, y / h));
    let mut events: Vec<(f64, (f64, f64))> = Vec::new();
    if click {
        events.extend(track.clicks().filter_map(|t| Some((t, at(t)?))));
    }
    if hover {
        // Resting = staying within a small radius, relative to the screen.
        let radius = (bounds.width.max(bounds.height) as f64 / 100.0).max(8.0);
        for (from, to) in rests(track, radius, cfg.dwell_secs) {
            // Both ends, so the zoom holds for the whole rest.
            if let Some(p) = at(from) {
                events.push((from, p));
                events.push((to, p));
            }
        }
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total = track.samples.last().map_or(0.0, |s| s.t);
    let depths = cfg.depths();
    let ramp = |s: f64| cfg.ramp(s);
    let rules = shots::Rules {
        depths: &depths,
        tail: cfg.hold_secs.max(0.0),
        ramp: &ramp,
    };
    shots::plan(&events, total, &rules)
        .into_iter()
        .map(|s| {
            let half = 0.5 / s.scale;
            let (x, y) = s.bbox.centre();
            Zoom {
                start: s.start,
                end: s.end,
                scale: s.scale,
                ramp: cfg.ramp(s.scale),
                focus: (x.clamp(half, 1.0 - half), y.clamp(half, 1.0 - half)),
            }
        })
        .collect()
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

    /// A take of `secs` with clicks at `(t, x, y)`, the pointer gliding
    /// between them.
    fn clicks(secs: f64, clicks: &[(f64, f64, f64)]) -> CursorTrack {
        let mut s = vec![(0.0, 500.0, 300.0, false)];
        for &(t, x, y) in clicks {
            s.push((t - 0.3, x, y, false));
            s.push((t, x, y, true));
            s.push((t + 0.12, x, y, false));
        }
        s.push((secs, s[s.len() - 1].1, s[s.len() - 1].2, false));
        track(&s)
    }

    fn views(p: &mut ZoomPlanner, secs: f64) -> Vec<Rect> {
        (0..=(secs * 30.0) as usize)
            .map(|i| p.next(i as f64 / 30.0))
            .collect()
    }

    fn zoom_of(r: &Rect) -> f64 {
        1000.0 / r.width
    }

    const BOUNDS: Size = Size {
        width: 1000,
        height: 600,
    };

    #[test]
    fn no_clicks_means_no_zoom() {
        let tr = track(&[(0.0, 100.0, 100.0, false), (5.0, 900.0, 500.0, false)]);
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, BOUNDS);
        assert!(views(&mut p, 5.0).iter().all(|r| *r == Rect::full(BOUNDS)));
    }

    #[test]
    fn click_zooms_in_then_out() {
        let tr = clicks(12.0, &[(5.0, 800.0, 400.0)]);
        let p = ZoomPlanner::new(ZoomConfig::default(), &tr, BOUNDS);
        let at_click = p.view_at(5.0);
        assert!((zoom_of(&at_click) - 1.8).abs() < 1e-9, "{at_click:?}");
        // Framed on the click, inside the screen.
        assert!(at_click.x < 800.0 && at_click.x + at_click.width > 800.0);
        assert!(at_click.x + at_click.width <= 1000.0);
        assert_eq!(p.view_at(9.5), Rect::full(BOUNDS));
    }

    #[test]
    fn zoom_starts_and_ends_gently() {
        let tr = clicks(12.0, &[(5.0, 500.0, 300.0)]);
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, BOUNDS);
        let widths: Vec<f64> = views(&mut p, 12.0).iter().map(|r| r.width).collect();
        let steps: Vec<f64> = widths.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        let biggest = steps.iter().cloned().fold(0.0, f64::max);
        let first = steps.iter().position(|&s| s > 1e-9).unwrap();
        let last = steps.iter().rposition(|&s| s > 1e-9).unwrap();
        assert!(
            steps[first] < biggest * 0.2,
            "{} vs {biggest}",
            steps[first]
        );
        assert!(steps[last] < biggest * 0.2, "{} vs {biggest}", steps[last]);
        // A calm move: at least ~0.8 s each way at 30 fps.
        let moving = steps.iter().filter(|&&s| s > 1e-9).count();
        assert!(moving >= 2 * 24, "{moving} moving frames");
        // Never past full zoom, never wider than the screen.
        assert!(widths
            .iter()
            .all(|w| (1000.0 / 1.8 - 1e-9..=1000.0).contains(w)));
    }

    #[test]
    fn settles_exactly_so_still_frames_are_identical() {
        let tr = clicks(12.0, &[(5.0, 600.0, 300.0)]);
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, BOUNDS);
        let v = views(&mut p, 12.0);
        // Fully in from 0.5 s before the click until 1.5 s after (or the
        // 1.8 s minimum hold).
        let held = &v[(4.5 * 30.0) as usize..=(6.3 * 30.0) as usize];
        assert!(held.iter().all(|r| *r == held[0]), "{held:?}");
        assert!(v[(8.0 * 30.0) as usize..]
            .iter()
            .all(|r| *r == Rect::full(BOUNDS)));
    }

    #[test]
    fn the_camera_holds_still_across_nearby_clicks() {
        // Click a text field, then its button: one zoom framing both, with
        // no pan in between.
        let tr = clicks(14.0, &[(4.0, 300.0, 100.0), (6.5, 650.0, 100.0)]);
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, BOUNDS);
        let v = views(&mut p, 14.0);
        let held = &v[(3.5 * 30.0) as usize..=(8.0 * 30.0) as usize];
        assert!(held.iter().all(|r| *r == held[0]), "panned");
        let r = held[0];
        assert!(
            r.x < 300.0 && r.x + r.width > 650.0,
            "both clicks in view: {r:?}"
        );
        assert!(
            (zoom_of(&r) - 1.5).abs() < 1e-9,
            "depth fits the pair: {r:?}"
        );
    }

    #[test]
    fn no_pumping_between_zooms() {
        // Clicks all over the place, every second: the camera must never
        // zoom out and straight back in.
        let pts = [
            (3.0, 100.0, 100.0),
            (4.0, 900.0, 100.0),
            (5.0, 900.0, 500.0),
            (6.0, 100.0, 500.0),
            (7.0, 500.0, 300.0),
            (8.0, 120.0, 80.0),
        ];
        let tr = clicks(14.0, &pts);
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, BOUNDS);
        let zoom: Vec<f64> = views(&mut p, 14.0).iter().map(zoom_of).collect();
        // Count direction changes of the zoom level: in, then out, per zoom.
        let mut turns = 0;
        let mut dir = 0.0;
        for w in zoom.windows(2) {
            let d = w[1] - w[0];
            if d.abs() > 1e-9 && d.signum() != dir {
                turns += 1;
                dir = d.signum();
            }
        }
        assert!(turns <= 2, "{turns} turns: {zoom:?}");
    }

    #[test]
    fn early_clicks_wait_for_the_opening() {
        let tr = clicks(10.0, &[(0.8, 500.0, 300.0), (3.8, 520.0, 300.0)]);
        let mut p = ZoomPlanner::new(ZoomConfig::default(), &tr, BOUNDS);
        let v = views(&mut p, 10.0);
        assert!(v[..(2.5 * 30.0) as usize]
            .iter()
            .all(|r| *r == Rect::full(BOUNDS)));
        assert!(zoom_of(&p.view_at(3.8)) > 1.7);
    }

    #[test]
    fn zoom_level_caps_the_depth() {
        let tr = clicks(12.0, &[(5.0, 500.0, 300.0)]);
        let cfg = ZoomConfig {
            level: 1.25,
            ..Default::default()
        };
        let p = ZoomPlanner::new(cfg, &tr, BOUNDS);
        assert!((zoom_of(&p.view_at(5.0)) - 1.25).abs() < 1e-9);
        let cfg = ZoomConfig {
            level: 2.5,
            ..Default::default()
        };
        let p = ZoomPlanner::new(cfg, &tr, BOUNDS);
        assert!((zoom_of(&p.view_at(5.0)) - 2.5).abs() < 1e-9);
    }

    #[test]
    fn hover_zooms_where_the_cursor_rests() {
        // Moving, then resting at (200, 150) for a second, then moving on.
        let mut s = Vec::new();
        for i in 0..=20 {
            s.push((
                3.0 + i as f64 * 0.05,
                900.0 - i as f64 * 35.0,
                500.0 - i as f64 * 17.5,
                false,
            ));
        }
        for i in 1..=20 {
            s.push((4.0 + i as f64 * 0.05, 200.0, 150.0, false));
        }
        s.push((7.0, 900.0, 500.0, false));
        s.push((12.0, 900.0, 500.0, false));
        let tr = track(&s);
        let cfg = ZoomConfig {
            trigger: ZoomTrigger::Hover,
            ..Default::default()
        };
        let p = ZoomPlanner::new(cfg, &tr, BOUNDS);
        let r = p.view_at(4.8);
        assert!(zoom_of(&r) > 1.7, "zoomed in: {r:?}");
        assert!(
            r.x < 200.0 && r.x + r.width > 200.0,
            "around the rest: {r:?}"
        );

        let p = ZoomPlanner::new(ZoomConfig::default(), &tr, BOUNDS);
        assert_eq!(p.view_at(4.8), Rect::full(BOUNDS));
    }

    #[test]
    fn resting_where_recording_began_is_not_hover() {
        let tr = track(&[
            (0.0, 900.0, 500.0, false),
            (4.0, 900.0, 500.0, false),
            (4.5, 100.0, 100.0, false),
            (4.6, 400.0, 300.0, false),
        ]);
        let cfg = ZoomConfig {
            trigger: ZoomTrigger::Hover,
            ..Default::default()
        };
        assert!(plan(&cfg, &tr, BOUNDS).is_empty());
    }

    #[test]
    fn small_movements_dont_pan_in_follow_mode() {
        let mut s = vec![(0.0, 500.0, 300.0, false)];
        for i in 0..60 {
            let d = if i % 2 == 0 { 30.0 } else { -30.0 };
            s.push((0.2 + i as f64 * 0.05, 500.0 + d, 300.0, false));
        }
        let tr = track(&s);
        let cfg = ZoomConfig {
            trigger: ZoomTrigger::Follow,
            ..Default::default()
        };
        let p = ZoomPlanner::new(cfg, &tr, BOUNDS);
        let (a, b) = (p.view_at(1.5), p.view_at(3.0));
        assert!((a.x - b.x).abs() < 1.0, "{a:?} {b:?}");
    }

    #[test]
    fn follow_mode_tracks_the_cursor_and_settles() {
        let tr = track(&[(0.0, 0.0, 0.0, false), (1.0, 1000.0, 600.0, false)]);
        let cfg = ZoomConfig {
            trigger: ZoomTrigger::Follow,
            level: 2.0,
            ..Default::default()
        };
        let p = ZoomPlanner::new(cfg, &tr, BOUNDS);
        let r = p.view_at(6.0);
        assert!((r.width - 500.0).abs() < 1e-9);
        assert!(
            (r.x - 500.0).abs() < 1e-6 && (r.y - 300.0).abs() < 1e-6,
            "{r:?}"
        );
        assert_eq!(p.view_at(5.0), p.view_at(6.0));
    }
}
