use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Scale to `width`, keeping aspect ratio, rounding both sides to even
    /// numbers (required by yuv420p encoders).
    pub fn fit_width(self, width: u32) -> Size {
        let width = width.min(self.width).max(2);
        let height = (self.height as f64 * width as f64 / self.width as f64).round() as u32;
        Size::new(even(width), even(height.max(2)))
    }
}

fn even(v: u32) -> u32 {
    v & !1
}

/// Axis-aligned rectangle in source-pixel space (floating point so the
/// zoom camera can move smoothly at sub-pixel precision).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn full(size: Size) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: size.width as f64,
            height: size.height as f64,
        }
    }

    /// A rect of `bounds`' aspect ratio, `1/zoom` its size, centred on
    /// (`cx`, `cy`) and shifted so it never leaves `bounds`.
    pub fn zoomed(bounds: Size, zoom: f64, cx: f64, cy: f64) -> Self {
        let zoom = zoom.max(1.0);
        let (bw, bh) = (bounds.width as f64, bounds.height as f64);
        let (w, h) = (bw / zoom, bh / zoom);
        let x = (cx - w / 2.0).clamp(0.0, bw - w);
        let y = (cy - h / 2.0).clamp(0.0, bh - h);
        Self {
            x,
            y,
            width: w,
            height: h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_width_keeps_aspect_and_even() {
        assert_eq!(Size::new(1920, 1080).fit_width(960), Size::new(960, 540));
        assert_eq!(Size::new(1001, 701).fit_width(5000), Size::new(1000, 700));
    }

    #[test]
    fn zoomed_rect_is_clamped_to_bounds() {
        let b = Size::new(1000, 500);
        let r = Rect::zoomed(b, 2.0, 0.0, 0.0);
        assert_eq!((r.x, r.y, r.width, r.height), (0.0, 0.0, 500.0, 250.0));
        let r = Rect::zoomed(b, 2.0, 1000.0, 500.0);
        assert_eq!((r.x, r.y), (500.0, 250.0));
        let r = Rect::zoomed(b, 0.5, 10.0, 10.0);
        assert_eq!(r, Rect::full(b));
    }
}
