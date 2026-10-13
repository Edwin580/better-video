//! When to zoom, where, and how deep: turns the clicks (or rests) of a
//! take into a few calm zoom "shots", the way an editor would.
//!
//! The video opens on the whole screen. Clicks close in time and in space
//! share one zoom, deep enough to show them all with a margin and no
//! deeper. A zoom never comes straight back after leaving: two zooms
//! either have a real wide shot between them, merge into one, or the
//! weaker goes. The video ends on the whole screen.
//!
//! Ported from OpenScreen (https://github.com/getopenscreen/openscreen),
//! src/lib/ai-edition/timeline/zoom-suggestions.ts (`planShots`,
//! `resolveShots`, `fitDepth`), MIT License,
//! Copyright (c) 2025 Siddharth Vaddem,
//! Copyright (c) 2025-2026 OpenScreen contributors.
//! See THIRD-PARTY-NOTICES.md. Changes: seconds instead of ms, one clip, no
//! zooms placed by hand, the depths, tail and ramp length are inputs, and a
//! zoom is never in while a click lands outside its view (`clear_window`).

/// The video opens on the whole screen: no zoom starts moving in before this.
pub const OPENING_WIDE_S: f64 = 2.5;
/// And it ends on it: the last zoom is fully back out this long before the end.
pub const CLOSING_WIDE_S: f64 = 1.0;
/// A click this close to the end is the one that stopped the recording, or
/// one whose result the video never shows.
pub const STOP_CLICK_GUARD_S: f64 = 1.0;
/// Clicks closer than this in time share one zoom, when they fit in it together.
pub const SHOT_MERGE_GAP_S: f64 = 3.0;
/// A zoom is fully in this long before its first click...
pub const SHOT_LEAD_S: f64 = 0.5;
/// The shortest time a zoom holds, fully in.
pub const MIN_SHOT_HOLD_S: f64 = 1.8;
/// The shortest wide shot between two zooms, ramps excluded. Less reads as a yo-yo.
pub const MIN_WIDE_BETWEEN_S: f64 = 1.5;
/// The margin kept around the clicks a zoom frames, as a fraction of the frame.
pub const SHOT_FIT_MARGIN: f64 = 0.12;
/// The depths a zoom may take, shallowest first.
pub const SHOT_DEPTHS: [f64; 3] = [1.25, 1.5, 1.8];

/// Where a group of events falls, in 0..1 of the frame on each axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Box2 {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
}

impl Box2 {
    fn around((x, y): (f64, f64)) -> Self {
        Self {
            min_x: x,
            max_x: x,
            min_y: y,
            max_y: y,
        }
    }

    fn union(self, o: Self) -> Self {
        Self {
            min_x: self.min_x.min(o.min_x),
            max_x: self.max_x.max(o.max_x),
            min_y: self.min_y.min(o.min_y),
            max_y: self.max_y.max(o.max_y),
        }
    }

    pub fn centre(&self) -> (f64, f64) {
        (
            (self.min_x + self.max_x) / 2.0,
            (self.min_y + self.max_y) / 2.0,
        )
    }
}

/// One planned zoom: fully in over `start..=end` (its ramps lie outside).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shot {
    pub start: f64,
    pub end: f64,
    pub scale: f64,
    pub bbox: Box2,
    /// How many events it frames: the weight when two shots compete.
    pub events: usize,
}

/// What the planner needs besides the events.
#[derive(Clone, Copy)]
pub struct Rules<'a> {
    /// Allowed magnifications, shallowest first.
    pub depths: &'a [f64],
    /// How long a zoom holds after its last event.
    pub tail: f64,
    /// Seconds a move to `scale` takes.
    pub ramp: &'a dyn Fn(f64) -> f64,
}

/// The deepest allowed depth that shows the whole box with its margin.
fn fit_depth(b: Box2, depths: &[f64]) -> Option<f64> {
    let size = (b.max_x - b.min_x).max(b.max_y - b.min_y) + 2.0 * SHOT_FIT_MARGIN;
    depths.iter().copied().rfind(|d| d * size <= 1.0)
}

/// Plan the zooms for `events` (time, point in 0..1), sorted by time, in a
/// clip `total` seconds long.
pub fn plan(events: &[(f64, (f64, f64))], total: f64, rules: &Rules) -> Vec<Shot> {
    let events: Vec<_> = events
        .iter()
        .copied()
        .filter(|e| e.0 < total - STOP_CLICK_GUARD_S)
        .collect();
    let mut shots = plan_shots(&events, total, rules);
    resolve(&mut shots, &events, rules);
    shots.sort_by(|a, b| a.start.total_cmp(&b.start));
    shots
}

/// The part of the frame a zoom of `scale` framing `bbox` shows, as
/// `(min_x, max_x, min_y, max_y)`.
fn view(bbox: Box2, scale: f64) -> Box2 {
    let half = 0.5 / scale;
    let (x, y) = bbox.centre();
    let (x, y) = (x.clamp(half, 1.0 - half), y.clamp(half, 1.0 - half));
    Box2 {
        min_x: x - half,
        max_x: x + half,
        min_y: y - half,
        max_y: y + half,
    }
}

fn outside(v: Box2, (x, y): (f64, f64)) -> bool {
    x < v.min_x || x > v.max_x || y < v.min_y || y > v.max_y
}

/// The window, ramps included, a zoom framing `bbox` at `scale` may use
/// around the events `first..=last`: it is back out before any event it
/// would hide, and only comes in after the last one before it.
/// (Not in OpenScreen: there a zoom could hold while the next click lands
/// off screen.)
fn clear_window(
    events: &[(f64, (f64, f64))],
    bbox: Box2,
    scale: f64,
    first: f64,
    last: f64,
) -> (f64, f64) {
    let v = view(bbox, scale);
    let hidden = |e: &&(f64, (f64, f64))| outside(v, e.1);
    let lo = events
        .iter()
        .filter(|e| e.0 < first)
        .filter(hidden)
        .map(|e| e.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let hi = events
        .iter()
        .filter(|e| e.0 > last)
        .filter(hidden)
        .map(|e| e.0)
        .fold(f64::INFINITY, f64::min);
    (lo, hi)
}

/// Each group of events as a timed shot. A shot the opening and ending (or
/// clicks it would hide) leave too short a hold, or none of its events, is
/// dropped: a zoom arriving after the fact shows nothing.
fn plan_shots(events: &[(f64, (f64, f64))], total: f64, rules: &Rules) -> Vec<Shot> {
    let mut groups: Vec<(Vec<f64>, Box2)> = Vec::new();
    for &(t, p) in events {
        if let Some((times, bbox)) = groups.last_mut() {
            if t - times[times.len() - 1] <= SHOT_MERGE_GAP_S {
                let merged = bbox.union(Box2::around(p));
                if fit_depth(merged, rules.depths).is_some() {
                    times.push(t);
                    *bbox = merged;
                    continue;
                }
            }
        }
        groups.push((vec![t], Box2::around(p)));
    }

    let mut shots = Vec::new();
    for (times, bbox) in groups {
        // A group only grows while it fits; a lone point that doesn't fit
        // means no depth is allowed at all.
        let Some(scale) = fit_depth(bbox, rules.depths) else {
            continue;
        };
        let ramp = (rules.ramp)(scale);
        let (first, last) = (times[0], times[times.len() - 1]);
        let (lo, hi) = clear_window(events, bbox, scale, first, last);
        let earliest = (OPENING_WIDE_S + ramp).max(lo + ramp).max(0.0);
        let latest = (total - CLOSING_WIDE_S - ramp).min(hi - ramp).min(total);
        // Near the end, the zoom arrives earlier rather than holding too briefly.
        let start = (first - SHOT_LEAD_S)
            .min(latest - MIN_SHOT_HOLD_S)
            .max(earliest);
        let end = (last + rules.tail).max(start + MIN_SHOT_HOLD_S).min(latest);
        if end - start < MIN_SHOT_HOLD_S - 1e-9 {
            continue;
        }
        if !times.iter().any(|&t| t >= start && t <= end) {
            continue;
        }
        shots.push(Shot {
            start,
            end,
            scale,
            bbox,
            events: times.len(),
        });
    }
    shots
}

/// No yo-yo: between two zooms, a real wide shot or none at all. Two shots
/// too close merge when they fit in one zoom together (and it hides no
/// click); otherwise the one with fewer events goes, the later one on a
/// tie.
fn resolve(plan: &mut Vec<Shot>, events: &[(f64, (f64, f64))], rules: &Rules) {
    loop {
        plan.sort_by(|a, b| a.start.total_cmp(&b.start));
        let out_of = |s: &Shot| s.end + (rules.ramp)(s.scale);
        // Each shot against the one before it that ends latest, ramp included.
        let mut reach: Option<usize> = None;
        let mut clash = None;
        for i in 0..plan.len() {
            if let Some(r) = reach {
                let wide = plan[i].start - out_of(&plan[r]) - (rules.ramp)(plan[i].scale);
                if wide < MIN_WIDE_BETWEEN_S {
                    clash = Some((r, i));
                    break;
                }
            }
            if reach.is_none_or(|r| out_of(&plan[i]) > out_of(&plan[r])) {
                reach = Some(i);
            }
        }
        let Some((a, b)) = clash else { return };
        let bbox = plan[a].bbox.union(plan[b].bbox);
        // A merged zoom must not hide a click between the two either.
        let merged = fit_depth(bbox, rules.depths).filter(|&scale| {
            let ramp = (rules.ramp)(scale);
            let (from, to) = (plan[a].start - ramp, plan[a].end.max(plan[b].end) + ramp);
            let v = view(bbox, scale);
            !events
                .iter()
                .any(|e| e.0 >= from && e.0 <= to && outside(v, e.1))
        });
        if let Some(scale) = merged {
            plan[a] = Shot {
                start: plan[a].start,
                end: plan[a].end.max(plan[b].end),
                scale,
                bbox,
                events: plan[a].events + plan[b].events,
            };
            plan.remove(b);
        } else if plan[a].events >= plan[b].events {
            plan.remove(b);
        } else {
            plan.remove(a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zoom::envelope::zoom_transition_s;

    fn rules() -> Rules<'static> {
        Rules {
            depths: &SHOT_DEPTHS,
            tail: 1.5,
            ramp: &zoom_transition_s,
        }
    }

    fn assert_no_yo_yo(shots: &[Shot]) {
        for w in shots.windows(2) {
            let wide = w[1].start
                - zoom_transition_s(w[1].scale)
                - w[0].end
                - zoom_transition_s(w[0].scale);
            assert!(wide >= MIN_WIDE_BETWEEN_S - 1e-9, "{shots:?}");
        }
    }

    #[test]
    fn nearby_clicks_share_one_zoom_that_fits_them() {
        // Click an input, then its button a third of the screen away.
        let ev = [(4.0, (0.4, 0.2)), (6.0, (0.75, 0.2))];
        let shots = plan(&ev, 12.0, &rules());
        assert_eq!(shots.len(), 1, "{shots:?}");
        let s = shots[0];
        // 0.35 + 2 * 0.12 = 0.59 of the frame: 1.5x fits, 1.8x doesn't.
        assert_eq!(s.scale, 1.5);
        assert_eq!((s.start, s.end), (3.5, 7.5));
    }

    #[test]
    fn a_lone_click_gets_the_deepest_zoom() {
        let shots = plan(&[(5.0, (0.5, 0.5))], 12.0, &rules());
        assert_eq!(shots.len(), 1);
        assert_eq!(shots[0].scale, 1.8);
        // Holds at least the minimum, not just the 1.5 s tail.
        assert!(shots[0].end - shots[0].start >= MIN_SHOT_HOLD_S);
    }

    #[test]
    fn clicks_across_the_whole_screen_dont_ping_pong() {
        // Four quick clicks in the four corners: no one zoom fits them, and
        // zooming to each in turn would be a yo-yo.
        let ev = [
            (4.0, (0.05, 0.05)),
            (4.8, (0.95, 0.05)),
            (5.6, (0.95, 0.95)),
            (6.4, (0.05, 0.95)),
        ];
        let shots = plan(&ev, 14.0, &rules());
        assert!(shots.len() <= 1, "{shots:?}");
        assert_no_yo_yo(&shots);
    }

    #[test]
    fn the_opening_and_ending_stay_wide() {
        // A click straight away: the zoom waits for the opening.
        let shots = plan(&[(0.6, (0.5, 0.5)), (3.6, (0.5, 0.5))], 10.0, &rules());
        assert_eq!(shots.len(), 1);
        assert!(shots[0].start - zoom_transition_s(shots[0].scale) >= OPENING_WIDE_S - 1e-9);
        // A click right at the end is ignored (it's the one that stopped it).
        assert!(plan(&[(9.5, (0.5, 0.5))], 10.0, &rules()).is_empty());
        // One just before the end arrives early and is out before the end.
        let s = plan(&[(7.5, (0.5, 0.5))], 10.0, &rules());
        assert_eq!(s.len(), 1);
        assert!(s[0].end + zoom_transition_s(s[0].scale) <= 10.0 - CLOSING_WIDE_S + 1e-9);
    }

    #[test]
    fn zooms_too_close_merge_or_the_weaker_goes() {
        // Same spot, 3.5 s apart: one zoom, not out and back in.
        let shots = plan(&[(4.0, (0.3, 0.3)), (7.5, (0.3, 0.3))], 14.0, &rules());
        assert_eq!(shots.len(), 1, "{shots:?}");
        assert_eq!(shots[0].events, 2);
        // Far apart on screen and in time: two zooms with a real wide shot.
        let ev = [(4.0, (0.1, 0.1)), (11.0, (0.9, 0.9))];
        let shots = plan(&ev, 16.0, &rules());
        assert_eq!(shots.len(), 2, "{shots:?}");
        assert_no_yo_yo(&shots);
        // Far apart on screen, close in time, two clicks against one: the
        // pair keeps its zoom.
        let ev = [(4.0, (0.1, 0.1)), (4.5, (0.1, 0.15)), (7.0, (0.9, 0.9))];
        let shots = plan(&ev, 16.0, &rules());
        assert_eq!(shots.len(), 1, "{shots:?}");
        assert_eq!(shots[0].events, 2);
    }

    #[test]
    fn a_zoom_never_hides_a_click() {
        // A click at the right, then 1.5 s later one at the far left: a
        // zoom on the first would still be in when the second lands off
        // screen, so it is cut short, and dropped if that leaves too
        // little; the second gets its zoom once the first is out of the way.
        let ev = [(4.0, (0.9, 0.5)), (5.5, (0.1, 0.5))];
        let shots = plan(&ev, 14.0, &rules());
        for s in &shots {
            let v = view(s.bbox, s.scale);
            let ramp = zoom_transition_s(s.scale);
            for e in &ev {
                let zoomed = e.0 >= s.start - ramp && e.0 <= s.end + ramp;
                assert!(!(zoomed && outside(v, e.1)), "{e:?} hidden by {s:?}");
            }
        }
        assert_eq!(shots.len(), 1, "{shots:?}");
        assert_eq!(shots[0].bbox.min_x, 0.1);
    }

    #[test]
    fn depth_is_capped_by_the_allowed_depths() {
        let r = Rules {
            depths: &[1.25],
            ..rules()
        };
        assert_eq!(plan(&[(5.0, (0.5, 0.5))], 12.0, &r)[0].scale, 1.25);
        let none = Rules {
            depths: &[],
            ..rules()
        };
        assert!(plan(&[(5.0, (0.5, 0.5))], 12.0, &none).is_empty());
    }
}
