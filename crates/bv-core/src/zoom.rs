//! Cursor-following "camera" that decides which part of the screen each
//! exported frame shows.

use serde::{Deserialize, Serialize};

use crate::{cursor::CursorTrack, geometry::Rect, geometry::Size};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ZoomTrigger {
    /// Zoom in around each click, ease back out after `hold_secs` idle.
    #[default]
    Click,
    /// Stay zoomed in and follow the cursor for the whole clip.
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ZoomConfig {
    /// Magnification when zoomed in (2.0 = show a quarter of the screen).
    pub level: f64,
    pub trigger: ZoomTrigger,
    /// How long to stay zoomed after the last click.
    pub hold_secs: f64,
    /// Time constant (seconds) for the camera easing toward its target.
    /// Smaller is snappier; larger is smoother.
    pub smoothing_secs: f64,
}

impl Default for ZoomConfig {
    fn default() -> Self {
        Self {
            level: 2.0,
            trigger: ZoomTrigger::Click,
            hold_secs: 1.5,
            smoothing_secs: 0.25,
        }
    }
}

/// Produces one viewport per output frame. Stateful (it eases toward the
/// target), so frames must be requested in order via [`ZoomPlanner::next`].
pub struct ZoomPlanner<'a> {
    cfg: ZoomConfig,
    track: &'a CursorTrack,
    bounds: Size,
    clicks: Vec<f64>,
    zoom: f64,
    cx: f64,
    cy: f64,
    last_t: Option<f64>,
}

impl<'a> ZoomPlanner<'a> {
    pub fn new(cfg: ZoomConfig, track: &'a CursorTrack, bounds: Size) -> Self {
        let mut clicks: Vec<f64> = track.clicks().collect();
        clicks.sort_by(f64::total_cmp);
        let (cx, cy) = track
            .position_at(0.0)
            .unwrap_or((bounds.width as f64 / 2.0, bounds.height as f64 / 2.0));
        let zoom = if cfg.trigger == ZoomTrigger::Always {
            cfg.level
        } else {
            1.0
        };
        Self {
            cfg,
            track,
            bounds,
            clicks,
            zoom,
            cx,
            cy,
            last_t: None,
        }
    }

    fn target_zoom(&self, t: f64) -> f64 {
        match self.cfg.trigger {
            ZoomTrigger::Always => self.cfg.level,
            ZoomTrigger::Click => {
                // Most recent click at or before t (start zooming slightly
                // early so the click itself is already magnified).
                let lead = self.cfg.smoothing_secs;
                let i = self.clicks.partition_point(|&c| c <= t + lead);
                let active = i > 0 && t - self.clicks[i - 1] <= self.cfg.hold_secs;
                if active {
                    self.cfg.level
                } else {
                    1.0
                }
            }
        }
    }

    /// Viewport for the frame at time `t` (seconds). `t` must not decrease.
    pub fn next(&mut self, t: f64) -> Rect {
        let (tx, ty) = self.track.position_at(t).unwrap_or((self.cx, self.cy));
        let tz = self.target_zoom(t);
        let dt = self.last_t.map_or(0.0, |last| (t - last).max(0.0));
        self.last_t = Some(t);

        // Frame-rate independent exponential easing.
        let k = if self.cfg.smoothing_secs <= 0.0 {
            1.0
        } else {
            1.0 - (-dt / self.cfg.smoothing_secs).exp()
        };
        self.zoom += (tz - self.zoom) * k;
        self.cx += (tx - self.cx) * k;
        self.cy += (ty - self.cy) * k;

        Rect::zoomed(self.bounds, self.zoom, self.cx, self.cy)
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
        let zoomed = run(&mut p, 2.0);
        assert!((zoomed.width - 500.0).abs() < 5.0, "{zoomed:?}");
        // Camera hugs the cursor but stays inside the screen.
        assert!(zoomed.x + zoomed.width <= 1000.0 && zoomed.x > 400.0);

        let mut p = ZoomPlanner::new(cfg, &tr, bounds);
        let out = run(&mut p, 6.0);
        assert!((out.width - 1000.0).abs() < 1.0, "{out:?}");
    }

    #[test]
    fn always_mode_follows_cursor() {
        let tr = track(&[(0.0, 0.0, 0.0, false), (1.0, 1000.0, 600.0, false)]);
        let cfg = ZoomConfig {
            trigger: ZoomTrigger::Always,
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
}
