//! How a zoom moves in and out: an envelope over each zoom span and the
//! easing curve it rides.
//!
//! Ported from OpenScreen (https://github.com/getopenscreen/openscreen),
//! crates/compositor/src/regions.rs (`zoom_transition_s`, `ease_spring`,
//! `scale_lerp`, `zoom_region_strength`), MIT License,
//! Copyright (c) 2025 Siddharth Vaddem,
//! Copyright (c) 2025-2026 OpenScreen contributors.
//! See THIRD-PARTY-NOTICES.md. Changes: f64, no speed regions, trims or 3D
//! camera, and the transition length takes a speed factor.

/// A zoom transition lasts `BASE + PER_LN * ln(scale)` seconds: ~0.7 s to
/// 1.25x, ~0.9 s to 1.8x. It grows with the log of the scale, like the
/// perceived distance, with a floor so a small zoom doesn't rush past.
const ZOOM_TRANSITION_BASE_S: f64 = 0.6;
const ZOOM_TRANSITION_PER_LN_S: f64 = 0.55;
/// `omega * T` of the spring in [`ease_spring`]: high enough that the end
/// of the window is at rest, low enough not to freeze its second half.
const SPRING_OMEGA_T: f64 = 7.0;

/// Seconds a transition to (or from) `scale` takes.
pub fn zoom_transition_s(scale: f64) -> f64 {
    ZOOM_TRANSITION_BASE_S + ZOOM_TRANSITION_PER_LN_S * scale.max(1.0).ln()
}

/// Rise of a critically damped spring (no bounce), rescaled to land
/// exactly on 1 at the end of the window: zero speed at the start, so no
/// jolt, and a damped arrival.
pub fn ease_spring(t: f64) -> f64 {
    let x = SPRING_OMEGA_T * t.clamp(0.0, 1.0);
    let end = 1.0 - (1.0 + SPRING_OMEGA_T) * (-SPRING_OMEGA_T).exp();
    (1.0 - (1.0 + x) * (-x).exp()) / end
}

/// The scale between `a` and `b`, interpolated in log space: a zoom is
/// perceived as a ratio (1x to 2x reads like 2x to 4x), so the pace of a
/// transition doesn't depend on how deep it goes.
pub fn scale_lerp(a: f64, b: f64, t: f64) -> f64 {
    let a = a.max(1e-3);
    a * (b.max(1e-3) / a).powf(t)
}

/// How far into the zoom spanning `start..=end` the camera is at `t`: 0
/// outside, an ease-in of `window` seconds that lands exactly on `start`
/// (the zoom is in place when the span begins), 1 during the span, and an
/// ease-out of `window` seconds from `end`.
pub fn strength(start: f64, end: f64, window: f64, t: f64) -> f64 {
    if t < start - window || t > end + window {
        return 0.0;
    }
    if t < start {
        return ease_spring((t - (start - window)) / window);
    }
    if t <= end {
        return 1.0;
    }
    1.0 - ease_spring((t - end) / window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_lands_exactly_and_starts_still() {
        assert_eq!(ease_spring(0.0), 0.0);
        assert_eq!(ease_spring(1.0), 1.0);
        // Zero speed at the start: the first 2% of the time covers under 1%.
        assert!(ease_spring(0.02) < 0.01);
        let mut last = 0.0;
        for i in 1..=100 {
            let v = ease_spring(i as f64 / 100.0);
            assert!(v >= last && v <= 1.0, "monotonic, no overshoot");
            last = v;
        }
    }

    #[test]
    fn deeper_zooms_take_longer() {
        assert!((zoom_transition_s(1.25) - 0.72).abs() < 0.01);
        assert!((zoom_transition_s(1.8) - 0.92).abs() < 0.01);
        assert_eq!(zoom_transition_s(1.0), ZOOM_TRANSITION_BASE_S);
    }

    #[test]
    fn envelope_is_exact_at_the_edges() {
        assert_eq!(strength(2.0, 3.0, 0.5, 1.4), 0.0);
        assert_eq!(strength(2.0, 3.0, 0.5, 2.0), 1.0);
        assert_eq!(strength(2.0, 3.0, 0.5, 3.0), 1.0);
        assert_eq!(strength(2.0, 3.0, 0.5, 3.5), 0.0);
        assert_eq!(scale_lerp(1.0, 1.8, 1.0), 1.8);
        assert_eq!(scale_lerp(1.0, 1.8, 0.0), 1.0);
    }
}
