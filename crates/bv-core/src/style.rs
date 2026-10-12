//! Styled frame around the recording: a background (solid or gradient),
//! padding, rounded corners and a soft drop shadow.
//!
//! The background and shadow never change, so they're rendered once into a
//! base canvas. Each frame only copies the recording's changed rows on top
//! and blends the anti-aliased corners, which keeps the output identical
//! outside the recording from frame to frame (GIF delta encoding then
//! skips it entirely).

use std::{fmt, ops::Range, str::FromStr};

use crate::geometry::Size;

/// What's drawn behind the recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Background {
    Solid([u8; 3]),
    /// Diagonal gradient from the top-left to the bottom-right corner.
    Gradient([u8; 3], [u8; 3]),
}

impl Background {
    fn at(self, fx: f64, fy: f64) -> [f64; 3] {
        match self {
            Background::Solid(c) => c.map(f64::from),
            Background::Gradient(a, b) => {
                let t = ((fx + fy) / 2.0).clamp(0.0, 1.0);
                // Smoothstep: the colours ease in and out instead of meeting
                // at hard corners.
                let t = t * t * (3.0 - 2.0 * t);
                [0, 1, 2].map(|i| a[i] as f64 + (b[i] as f64 - a[i] as f64) * t)
            }
        }
    }
}

/// `#rrggbb`, or two of them separated by a comma for a gradient.
impl FromStr for Background {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let colors = s
            .split(',')
            .map(|c| parse_hex(c.trim()))
            .collect::<Result<Vec<_>, _>>()?;
        match colors[..] {
            [c] => Ok(Background::Solid(c)),
            [a, b] => Ok(Background::Gradient(a, b)),
            _ => Err("expected #rrggbb or #rrggbb,#rrggbb".into()),
        }
    }
}

impl fmt::Display for Background {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        match *self {
            Background::Solid(c) => f.write_str(&hex(c)),
            Background::Gradient(a, b) => write!(f, "{},{}", hex(a), hex(b)),
        }
    }
}

fn parse_hex(s: &str) -> Result<[u8; 3], String> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 || !h.is_ascii() {
        return Err(format!("bad colour {s:?}: expected #rrggbb"));
    }
    let byte =
        |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| format!("bad colour {s:?}"));
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

/// Built-in looks for `--style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StylePreset {
    /// Light neutral background.
    Clean,
    /// Blue to violet gradient.
    Gradient,
    /// Near-black slate background.
    Dark,
}

impl StylePreset {
    pub fn style(self) -> FrameStyle {
        let (background, shadow) = match self {
            StylePreset::Clean => (
                Background::Gradient([0xf1, 0xf3, 0xf6], [0xdc, 0xe0, 0xe6]),
                0.30,
            ),
            StylePreset::Gradient => (
                Background::Gradient([0x63, 0x8b, 0xf7], [0xa7, 0x7b, 0xf3]),
                0.40,
            ),
            StylePreset::Dark => (
                Background::Gradient([0x1e, 0x29, 0x3b], [0x10, 0x14, 0x1c]),
                0.60,
            ),
        };
        FrameStyle {
            background,
            padding: None,
            radius: None,
            shadow,
        }
    }
}

/// How the recording is framed. Sizes are in output pixels; `None` picks
/// one proportional to the recording.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameStyle {
    pub background: Background,
    /// Space between the recording and the edge of the output.
    pub padding: Option<u32>,
    /// Corner radius of the recording.
    pub radius: Option<u32>,
    /// Drop shadow opacity, 0 for none.
    pub shadow: f64,
}

/// Default padding, as a share of the recording's width.
const PADDING: f64 = 0.08;
/// Default corner radius, as a share of the recording's width.
const RADIUS: f64 = 0.022;

/// Where the recording sits in the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The whole output frame.
    pub canvas: Size,
    /// The recording, scaled.
    pub content: Size,
    /// Top-left corner of the recording on the canvas.
    pub x: u32,
    pub y: u32,
}

impl Layout {
    /// The output for a `src` recording fitted to `width` (the whole
    /// canvas, recording plus padding). Never scales the recording up.
    pub fn new(style: Option<&FrameStyle>, src: Size, width: Option<u32>) -> Layout {
        let Some(style) = style else {
            let content = src.fit_width(width.unwrap_or(src.width));
            return Layout {
                canvas: content,
                content,
                x: 0,
                y: 0,
            };
        };
        let pad_for = |cw: u32| match style.padding {
            Some(p) => p,
            None => (cw as f64 * PADDING).round() as u32,
        };
        let natural = src.width + 2 * pad_for(src.width);
        let (content, pad) = match width {
            Some(w) if w < natural => {
                let w = (w & !1).max(4);
                let cw = match style.padding {
                    Some(p) => w.saturating_sub(2 * p),
                    None => (w as f64 / (1.0 + 2.0 * PADDING)) as u32,
                };
                let content = src.fit_width(cw.max(2));
                (content, (w - content.width.min(w)) / 2)
            }
            _ => (src.fit_width(src.width), pad_for(src.width)),
        };
        Layout {
            canvas: Size::new(content.width + 2 * pad, content.height + 2 * pad),
            content,
            x: pad,
            y: pad,
        }
    }
}

/// A precomputed frame: the static background + shadow and the corner
/// coverage of the recording.
pub struct Frame {
    pub layout: Layout,
    base: Vec<u8>,
    /// Coverage (0–255) of the top-left `radius`×`radius` corner of the
    /// recording; the other corners mirror it.
    corner: Vec<u8>,
    radius: usize,
}

impl Frame {
    pub fn new(style: &FrameStyle, layout: Layout) -> Frame {
        let Layout {
            canvas, content, ..
        } = layout;
        let (cw, ch) = (content.width as f64, content.height as f64);
        let pad = layout.x as f64;
        let radius = style
            .radius
            .map_or((cw * RADIUS).round().max(4.0), f64::from)
            .min(cw.min(ch) / 2.0)
            .floor();

        // Corner coverage, 4×4 supersampled so the arc is smooth.
        let r = radius as usize;
        let mut corner = vec![255u8; r * r];
        for y in 0..r {
            for x in 0..r {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let px = x as f64 + (sx as f64 + 0.5) / 4.0;
                        let py = y as f64 + (sy as f64 + 0.5) / 4.0;
                        let (dx, dy) = (radius - px, radius - py);
                        if dx * dx + dy * dy <= radius * radius {
                            hits += 1;
                        }
                    }
                }
                corner[y * r + x] = (hits * 255 / 16) as u8;
            }
        }

        // Background with the shadow of the recording on it: a blurred
        // rounded rectangle, shifted down a little as if lit from above.
        let sigma = (pad * 0.4).max(1.0);
        let drop = (pad * 0.2).round();
        let (w, h) = (canvas.width as usize, canvas.height as usize);
        let mut base = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                let bg = style
                    .background
                    .at(px / canvas.width as f64, py / canvas.height as f64);
                let d = rounded_rect_distance(
                    px - pad - cw / 2.0,
                    py - pad - drop - ch / 2.0,
                    cw / 2.0,
                    ch / 2.0,
                    radius,
                );
                let a = style.shadow * (1.0 - normal_cdf(d / sigma));
                let i = (y * w + x) * 4;
                for c in 0..3 {
                    base[i + c] = (bg[c] * (1.0 - a)).round() as u8;
                }
            }
        }
        Frame {
            layout,
            base,
            corner,
            radius: r,
        }
    }

    /// Add a fixed ordered dither of about ±`step`/2 levels to the
    /// background, for encoders that quantize without dithering of their
    /// own (the native GIF path): a smooth gradient or shadow otherwise
    /// shows bands. The pattern depends only on the pixel position, so the
    /// background is still identical in every frame.
    pub fn dither(&mut self, step: f64) {
        let w = self.layout.canvas.width as usize;
        for (i, px) in self.base.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            let t = (bayer8(x, y) as f64 + 0.5) / 64.0 - 0.5;
            for c in &mut px[..3] {
                *c = (*c as f64 + t * step).round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    /// The canvas with nothing on it yet; [`Frame::compose`] draws the
    /// recording into it.
    pub fn blank(&self) -> Vec<u8> {
        self.base.clone()
    }

    /// Canvas rows covered by content rows `rows`.
    pub fn canvas_rows(&self, rows: Range<usize>) -> Range<usize> {
        let y = self.layout.y as usize;
        rows.start + y..rows.end + y
    }

    /// Copy rows `rows` of `content` (RGBA, `layout.content` sized) onto
    /// `canvas`, rounding the corners.
    pub fn compose(&self, content: &[u8], canvas: &mut [u8], rows: Range<usize>) {
        let Layout {
            canvas: cs,
            content: size,
            x: ox,
            y: oy,
        } = self.layout;
        let (cw, ch) = (size.width as usize, size.height as usize);
        let stride = cs.width as usize * 4;
        let r = self.radius;
        for y in rows.start.min(ch)..rows.end.min(ch) {
            let src = &content[y * cw * 4..(y + 1) * cw * 4];
            let start = (y + oy as usize) * stride + ox as usize * 4;
            let dst = &mut canvas[start..start + cw * 4];
            dst.copy_from_slice(src);
            // Distance into the corner band, if this row is in one.
            let cy = if y < r {
                y
            } else if y >= ch - r {
                ch - 1 - y
            } else {
                continue;
            };
            let base = &self.base[start..start + cw * 4];
            for i in 0..r {
                let cov = self.corner[cy * r + i] as u32;
                for x in [i, cw - 1 - i] {
                    let p = x * 4;
                    for c in 0..4 {
                        let (f, b) = (dst[p + c] as u32, base[p + c] as u32);
                        dst[p + c] = ((f * cov + b * (255 - cov) + 127) / 255) as u8;
                    }
                }
            }
        }
    }
}

/// 8×8 Bayer matrix entry (0–63) for a pixel.
fn bayer8(x: usize, y: usize) -> u32 {
    let (mut x, mut y) = (x as u32, y as u32);
    // The finest bits of x and y pick the most significant digit.
    let mut v = 0;
    for _ in 0..3 {
        v = (v << 2) | (((x & 1) ^ (y & 1)) << 1 | (y & 1));
        x >>= 1;
        y >>= 1;
    }
    v
}

/// Signed distance from a point (relative to the centre) to a rectangle of
/// half-size `hw`×`hh` with corner radius `r`; negative inside.
fn rounded_rect_distance(px: f64, py: f64, hw: f64, hh: f64, r: f64) -> f64 {
    let qx = px.abs() - (hw - r);
    let qy = py.abs() - (hh - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - r
}

/// Standard normal CDF, via the Abramowitz–Stegun erf approximation.
fn normal_cdf(x: f64) -> f64 {
    let z = x / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z.abs());
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-z * z).exp();
    0.5 * (1.0 + erf.copysign(z))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(buf: &[u8], size: Size, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * size.width + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2]]
    }

    fn solid(style: FrameStyle, src: Size, width: Option<u32>) -> (Frame, Vec<u8>) {
        let frame = Frame::new(&style, Layout::new(Some(&style), src, width));
        let c = frame.layout.content;
        let content = vec![0u8; (c.width * c.height * 4) as usize];
        let mut canvas = frame.blank();
        frame.compose(&content, &mut canvas, 0..c.height as usize);
        (frame, canvas)
    }

    #[test]
    fn none_is_a_no_op() {
        let src = Size::new(1920, 1080);
        let l = Layout::new(None, src, Some(1280));
        assert_eq!(l.canvas, src.fit_width(1280));
        assert_eq!(l.content, l.canvas);
        assert_eq!((l.x, l.y), (0, 0));
    }

    #[test]
    fn width_is_the_whole_canvas() {
        let style = StylePreset::Clean.style();
        let l = Layout::new(Some(&style), Size::new(1920, 1080), Some(1280));
        assert_eq!(l.canvas.width, 1280);
        assert_eq!(l.canvas.width, l.content.width + 2 * l.x);
        assert_eq!(l.canvas.height, l.content.height + 2 * l.y);
        assert!(l.x > 50 && l.x < 90, "{l:?}");
        assert_eq!(l.canvas.height % 2, 0);

        // A small recording isn't scaled up: the padding is added around it.
        let l = Layout::new(Some(&style), Size::new(560, 420), Some(1280));
        assert_eq!(l.content, Size::new(560, 420));
        assert_eq!(l.canvas.width, 560 + 2 * l.x);

        let fixed = FrameStyle {
            padding: Some(20),
            ..style
        };
        let l = Layout::new(Some(&fixed), Size::new(1000, 500), Some(640));
        assert_eq!((l.canvas.width, l.content.width, l.x), (640, 600, 20));
    }

    #[test]
    fn corners_show_background_and_centre_shows_content() {
        let style = FrameStyle {
            background: Background::Solid([200, 210, 220]),
            padding: Some(20),
            radius: Some(12),
            shadow: 0.0,
        };
        let (frame, canvas) = solid(style, Size::new(200, 100), None);
        let size = frame.layout.canvas;
        assert_eq!(size, Size::new(240, 140));
        // Outer corner of the canvas, and the recording's own corner pixel.
        assert_eq!(px(&canvas, size, 0, 0), [200, 210, 220]);
        assert_eq!(px(&canvas, size, 20, 20), [200, 210, 220]);
        assert_eq!(px(&canvas, size, 219, 119), [200, 210, 220]);
        // Centre and edge midpoints are the recording.
        assert_eq!(px(&canvas, size, 120, 70), [0, 0, 0]);
        assert_eq!(px(&canvas, size, 20, 70), [0, 0, 0]);
        // The arc is anti-aliased: some pixel along the diagonal is a blend.
        assert!((20..32).any(|d| {
            let p = px(&canvas, size, d, d)[0];
            p > 0 && p < 200
        }));
    }

    #[test]
    fn shadow_darkens_the_background_below_the_recording() {
        let style = FrameStyle {
            background: Background::Solid([240, 240, 240]),
            padding: Some(40),
            radius: Some(8),
            shadow: 0.4,
        };
        let (frame, canvas) = solid(style, Size::new(200, 100), None);
        let size = frame.layout.canvas;
        let below = px(&canvas, size, 140, 144)[0];
        let far = px(&canvas, size, 2, 2)[0];
        assert!(below < 225, "shadow under the recording: {below}");
        assert!(far >= 238, "no shadow far away: {far}");
        // Darker just under the edge than further out.
        assert!(px(&canvas, size, 140, 142)[0] < px(&canvas, size, 140, 170)[0]);
    }

    #[test]
    fn gradient_runs_corner_to_corner() {
        let style = FrameStyle {
            background: Background::Gradient([0, 0, 0], [255, 255, 255]),
            padding: Some(10),
            radius: Some(4),
            shadow: 0.0,
        };
        let (frame, canvas) = solid(style, Size::new(100, 100), None);
        let size = frame.layout.canvas;
        assert!(px(&canvas, size, 0, 0)[0] < 5);
        assert!(px(&canvas, size, 119, 119)[0] > 250);
    }

    #[test]
    fn partial_compose_matches_full() {
        let style = StylePreset::Gradient.style();
        let frame = Frame::new(&style, Layout::new(Some(&style), Size::new(64, 48), None));
        let c = frame.layout.content;
        let mut content: Vec<u8> = (0..c.width * c.height * 4)
            .map(|i| (i % 251) as u8)
            .collect();
        let mut partial = frame.blank();
        frame.compose(&content, &mut partial, 0..c.height as usize);
        content[(10 * c.width * 4) as usize..(12 * c.width * 4) as usize].fill(9);
        frame.compose(&content, &mut partial, 10..12);
        let mut full = frame.blank();
        frame.compose(&content, &mut full, 0..c.height as usize);
        assert_eq!(partial, full);
    }

    #[test]
    fn bayer_matrix() {
        let row: Vec<u32> = (0..8).map(|x| bayer8(x, 0)).collect();
        assert_eq!(row, [0, 32, 8, 40, 2, 34, 10, 42]);
        let mut all: Vec<u32> = (0..64).map(|i| bayer8(i % 8, i / 8)).collect();
        all.sort();
        assert_eq!(all, (0..64).collect::<Vec<_>>());
    }

    #[test]
    fn parses_backgrounds() {
        assert_eq!("#ff0080".parse(), Ok(Background::Solid([255, 0, 128])));
        assert_eq!(
            "#000000, fFfFfF".parse(),
            Ok(Background::Gradient([0; 3], [255; 3]))
        );
        assert!("red".parse::<Background>().is_err());
        assert!("#1,#2,#3".parse::<Background>().is_err());
    }
}
