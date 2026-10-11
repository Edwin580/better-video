use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One cursor observation, in screen pixels relative to the captured
/// region's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CursorSample {
    /// Seconds since the recording started.
    pub t: f64,
    pub x: f64,
    pub y: f64,
    /// True while any mouse button is held.
    #[serde(default)]
    pub pressed: bool,
}

/// Cursor path recorded alongside a video; saved as a JSON sidecar
/// (`<recording>.cursor.json`) so exports can be re-run with different
/// zoom settings without re-recording.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CursorTrack {
    pub samples: Vec<CursorSample>,
}

impl CursorTrack {
    pub fn push(&mut self, sample: CursorSample) {
        self.samples.push(sample);
    }

    /// Move every sample `dt` seconds earlier, e.g. to compensate for the
    /// delay before the first video frame.
    pub fn shift_earlier(&mut self, dt: f64) {
        for s in &mut self.samples {
            s.t -= dt;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Linearly interpolated position at time `t` (clamped to the ends).
    pub fn position_at(&self, t: f64) -> Option<(f64, f64)> {
        let s = &self.samples;
        let first = s.first()?;
        if t <= first.t {
            return Some((first.x, first.y));
        }
        let i = s.partition_point(|p| p.t <= t);
        if i >= s.len() {
            let last = s[s.len() - 1];
            return Some((last.x, last.y));
        }
        let (a, b) = (s[i - 1], s[i]);
        let span = b.t - a.t;
        let k = if span > 0.0 { (t - a.t) / span } else { 0.0 };
        Some((a.x + (b.x - a.x) * k, a.y + (b.y - a.y) * k))
    }

    /// Times at which a button went down.
    pub fn clicks(&self) -> impl Iterator<Item = f64> + '_ {
        self.samples
            .windows(2)
            .filter(|w| w[1].pressed && !w[0].pressed)
            .map(|w| w[1].t)
            .chain(self.samples.first().filter(|s| s.pressed).map(|s| s.t))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_slice(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fs::write(path, serde_json::to_vec(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: f64, x: f64, pressed: bool) -> CursorSample {
        CursorSample {
            t,
            x,
            y: x,
            pressed,
        }
    }

    #[test]
    fn interpolates_and_clamps() {
        let track = CursorTrack {
            samples: vec![s(1.0, 0.0, false), s(2.0, 100.0, false)],
        };
        assert_eq!(track.position_at(0.0), Some((0.0, 0.0)));
        assert_eq!(track.position_at(1.5), Some((50.0, 50.0)));
        assert_eq!(track.position_at(9.0), Some((100.0, 100.0)));
        assert_eq!(CursorTrack::default().position_at(1.0), None);
    }

    #[test]
    fn detects_press_edges() {
        let track = CursorTrack {
            samples: vec![
                s(0.0, 0.0, true),
                s(0.1, 0.0, false),
                s(0.2, 0.0, true),
                s(0.3, 0.0, true),
            ],
        };
        let mut clicks: Vec<_> = track.clicks().collect();
        clicks.sort_by(f64::total_cmp);
        assert_eq!(clicks, vec![0.0, 0.2]);
    }
}
