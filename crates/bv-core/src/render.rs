//! Per-frame crop + scale of raw RGBA frames.
//!
//! Every function takes the range of destination rows to produce, so when
//! only part of a recording changed (a line of typing, a blinking caret)
//! just those rows are redrawn.

use std::ops::Range;

use crate::geometry::{Rect, Size};

/// Sample `view` (a sub-rect of `src`) into rows `rows` of `dst`, scaling
/// with bilinear filtering. Both buffers are tightly packed RGBA8.
///
/// Crop and scale happen in one pass so no intermediate frame is allocated.
pub fn crop_scale_rgba(
    src: &[u8],
    src_size: Size,
    view: Rect,
    dst: &mut [u8],
    dst_size: Size,
    rows: Range<usize>,
) {
    let (sw, sh) = (src_size.width as usize, src_size.height as usize);
    let (dw, dh) = (dst_size.width as usize, dst_size.height as usize);
    assert_eq!(src.len(), sw * sh * 4, "src buffer size mismatch");
    assert_eq!(dst.len(), dw * dh * 4, "dst buffer size mismatch");
    let rows = rows.start.min(dh)..rows.end.min(dh);

    let sx = view.width / dw as f64;
    let sy = view.height / dh as f64;
    let max_x = (sw - 1) as f64;
    let max_y = (sh - 1) as f64;

    // Precompute horizontal taps once per frame; they're shared by all rows.
    let xs: Vec<(usize, usize, u32)> = (0..dw)
        .map(|x| {
            let fx = (view.x + (x as f64 + 0.5) * sx - 0.5).clamp(0.0, max_x);
            let x0 = fx as usize;
            let x1 = (x0 + 1).min(sw - 1);
            (x0 * 4, x1 * 4, ((fx - x0 as f64) * 256.0) as u32)
        })
        .collect();
    // Source bytes the taps touch: a zoomed view reads only part of a row.
    let lo = xs.first().map_or(0, |t| t.0.min(t.1));
    let hi = xs.last().map_or(0, |t| t.0.max(t.1) + 4);
    // One row blended vertically, then sampled horizontally: the vertical
    // pass is a straight run the compiler vectorizes, and the horizontal
    // one does half the work of filtering both ways per pixel. The sums
    // are the same products as a 2×2 filter, so results are identical.
    // At most 255·256 per entry, so it fits a u16.
    let mut line = vec![0u16; hi - lo];

    for (y, row) in dst
        .chunks_exact_mut(dw * 4)
        .enumerate()
        .skip(rows.start)
        .take(rows.len())
    {
        let fy = (view.y + (y as f64 + 0.5) * sy - 0.5).clamp(0.0, max_y);
        let y0 = fy as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let wy = ((fy - y0 as f64) * 256.0) as u16;
        let r0 = &src[y0 * sw * 4 + lo..y0 * sw * 4 + hi];
        let r1 = &src[y1 * sw * 4 + lo..y1 * sw * 4 + hi];
        for ((v, &a), &b) in line.iter_mut().zip(r0).zip(r1) {
            *v = a as u16 * (256 - wy) + b as u16 * wy;
        }

        for (px, &(x0, x1, wx)) in row.as_chunks_mut::<4>().0.iter_mut().zip(&xs) {
            let (a, b) = (&line[x0 - lo..][..4], &line[x1 - lo..][..4]);
            for c in 0..4 {
                let v = a[c] as u32 * (256 - wx) + b[c] as u32 * wx;
                px[c] = ((v + (1 << 15)) >> 16) as u8;
            }
        }
    }
}

/// Like [`crop_scale_rgba`], but when shrinking by 2× or more it samples at
/// a multiple of the output size and box-averages down, so small text stays
/// legible instead of aliasing. `scratch` is reused between calls.
pub fn crop_scale_rgba_smooth(
    src: &[u8],
    src_size: Size,
    view: Rect,
    dst: &mut [u8],
    dst_size: Size,
    rows: Range<usize>,
    scratch: &mut Vec<u8>,
) {
    let dh = dst_size.height as usize;
    let rows = rows.start.min(dh)..rows.end.min(dh);
    let k = ((view.width / dst_size.width as f64).floor() as u32).clamp(1, 4);
    if k == 1 {
        if view == Rect::full(src_size) && dst_size == src_size {
            let stride = src_size.width as usize * 4;
            let bytes = rows.start * stride..rows.end * stride;
            return dst[bytes.clone()].copy_from_slice(&src[bytes]);
        }
        return crop_scale_rgba(src, src_size, view, dst, dst_size, rows);
    }
    let big = Size::new(dst_size.width * k, dst_size.height * k);
    let k = k as usize;
    // Whole frame at an exact integer ratio (1920→960, 1920→640): the
    // bilinear pass would be an identity copy, so average straight away.
    if view == Rect::full(src_size) && big == src_size {
        return box_reduce(src, src_size, k, dst, dst_size, rows);
    }
    scratch.resize(big.width as usize * big.height as usize * 4, 0);
    let big_rows = rows.start * k..rows.end * k;
    crop_scale_rgba(src, src_size, view, scratch, big, big_rows);
    box_reduce(scratch, big, k, dst, dst_size, rows);
}

/// Average each `k×k` block of `src` into one pixel of rows `rows` of `dst`.
fn box_reduce(
    src: &[u8],
    src_size: Size,
    k: usize,
    dst: &mut [u8],
    dst_size: Size,
    rows: Range<usize>,
) {
    let (sw, dw) = (src_size.width as usize, dst_size.width as usize);
    let area = (k * k) as u32;
    let mut sums = vec![0u32; dw * 4];
    for (y, row) in dst
        .chunks_exact_mut(dw * 4)
        .enumerate()
        .skip(rows.start)
        .take(rows.len())
    {
        sums.fill(0);
        for sy in y * k..(y + 1) * k {
            let line = &src[sy * sw * 4..(sy + 1) * sw * 4];
            for (x, s) in sums.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                for px in line[x * k * 4..(x + 1) * k * 4].as_chunks::<4>().0 {
                    for c in 0..4 {
                        s[c] += px[c] as u32;
                    }
                }
            }
        }
        for (o, s) in row.iter_mut().zip(&sums) {
            *o = ((s + area / 2) / area) as u8;
        }
    }
}

/// Destination rows that can depend on source rows `src_rows` when `view`
/// is scaled to `dst_size` (with a margin for the filter taps).
pub fn affected_rows(src_rows: Range<u32>, view: Rect, dst_size: Size) -> Range<usize> {
    let scale = dst_size.height as f64 / view.height;
    let lo = ((src_rows.start as f64 - view.y) * scale).floor() as i64 - 2;
    let hi = ((src_rows.end as f64 - view.y) * scale).ceil() as i64 + 2;
    let clamp = |v: i64| v.clamp(0, dst_size.height as i64) as usize;
    clamp(lo)..clamp(hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(size: Size) -> Range<usize> {
        0..size.height as usize
    }

    #[test]
    fn smooth_downscale_averages() {
        // 4x2 checkerboard of black/white columns shrinks to mid grey.
        let src_size = Size::new(4, 2);
        let src: Vec<u8> = (0..8)
            .flat_map(|i| {
                let v = if i % 2 == 0 { 0 } else { 255 };
                [v, v, v, 255]
            })
            .collect();
        let out = Size::new(2, 1);
        let mut dst = vec![0; 8];
        let mut scratch = Vec::new();
        let view = Rect::full(src_size);
        crop_scale_rgba_smooth(&src, src_size, view, &mut dst, out, all(out), &mut scratch);
        for px in dst.chunks(4) {
            assert!((px[0] as i32 - 128).abs() <= 1, "{dst:?}");
            assert_eq!(px[3], 255);
        }
    }

    #[test]
    fn identity_copy() {
        let size = Size::new(3, 2);
        let src: Vec<u8> = (0..24).collect();
        let mut dst = vec![0; 24];
        crop_scale_rgba(&src, size, Rect::full(size), &mut dst, size, all(size));
        assert_eq!(src, dst);
    }

    #[test]
    fn crop_selects_quadrant() {
        // 4x4 image, each quadrant a solid colour.
        let size = Size::new(4, 4);
        let mut src = vec![0u8; 64];
        for y in 0..4 {
            for x in 0..4 {
                let v = if x >= 2 && y >= 2 { 200 } else { 10 };
                src[(y * 4 + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let out = Size::new(2, 2);
        let mut dst = vec![0; 16];
        let view = Rect {
            x: 2.0,
            y: 2.0,
            width: 2.0,
            height: 2.0,
        };
        crop_scale_rgba(&src, size, view, &mut dst, out, all(out));
        assert!(dst.chunks(4).all(|p| p == [200, 200, 200, 255]), "{dst:?}");
    }

    /// The straightforward 2×2 bilinear filter, per pixel.
    fn bilinear_reference(src: &[u8], ss: Size, view: Rect, ds: Size) -> Vec<u8> {
        let (sw, sh) = (ss.width as usize, ss.height as usize);
        let (dw, dh) = (ds.width as usize, ds.height as usize);
        let (sx, sy) = (view.width / dw as f64, view.height / dh as f64);
        let mut dst = vec![0u8; dw * dh * 4];
        for y in 0..dh {
            let fy = (view.y + (y as f64 + 0.5) * sy - 0.5).clamp(0.0, (sh - 1) as f64);
            let (y0, wy) = (fy as usize, ((fy - fy.floor()) * 256.0) as u32);
            let y1 = (y0 + 1).min(sh - 1);
            for x in 0..dw {
                let fx = (view.x + (x as f64 + 0.5) * sx - 0.5).clamp(0.0, (sw - 1) as f64);
                let (x0, wx) = (fx as usize, ((fx - fx.floor()) * 256.0) as u32);
                let x1 = (x0 + 1).min(sw - 1);
                for c in 0..4 {
                    let p = |x: usize, y: usize| src[(y * sw + x) * 4 + c] as u32;
                    let top = p(x0, y0) * (256 - wx) + p(x1, y0) * wx;
                    let bot = p(x0, y1) * (256 - wx) + p(x1, y1) * wx;
                    dst[(y * dw + x) * 4 + c] =
                        ((top * (256 - wy) + bot * wy + (1 << 15)) >> 16) as u8;
                }
            }
        }
        dst
    }

    #[test]
    fn separable_filter_matches_the_2x2_one_exactly() {
        let ss = Size::new(97, 61);
        let src: Vec<u8> = (0..97 * 61 * 4).map(|i| (i * 31 % 253) as u8).collect();
        let cases = [
            (Rect::full(ss), Size::new(97, 61)),
            (Rect::full(ss), Size::new(150, 90)),
            (Rect::full(ss), Size::new(60, 37)),
            (Rect::zoomed(ss, 1.8, 90.0, 5.0), Size::new(97, 61)),
            (Rect::zoomed(ss, 1.3, 10.0, 55.0), Size::new(131, 83)),
            (Rect::zoomed(ss, 2.5, 48.0, 30.0), Size::new(40, 25)),
        ];
        for (view, ds) in cases {
            let mut dst = vec![0u8; (ds.width * ds.height * 4) as usize];
            crop_scale_rgba(&src, ss, view, &mut dst, ds, all(ds));
            assert!(
                dst == bilinear_reference(&src, ss, view, ds),
                "{view:?} -> {ds:?}"
            );
        }
    }

    #[test]
    fn redrawing_affected_rows_matches_a_full_render() {
        // Change a band of source rows and redraw only the affected output
        // rows; the result must equal rendering everything again. Covers
        // the exact-ratio box path, the bilinear+box path and plain bilinear.
        let src_size = Size::new(96, 60);
        let views = [
            (Rect::full(src_size), Size::new(32, 20)),
            (Rect::zoomed(src_size, 2.0, 50.0, 30.0), Size::new(20, 12)),
            (Rect::zoomed(src_size, 1.7, 40.0, 25.0), Size::new(80, 50)),
        ];
        for (view, out) in views {
            let mut src: Vec<u8> = (0..96 * 60 * 4).map(|i| (i * 7 % 251) as u8).collect();
            let mut scratch = Vec::new();
            let mut partial = vec![0u8; (out.width * out.height * 4) as usize];
            crop_scale_rgba_smooth(
                &src,
                src_size,
                view,
                &mut partial,
                out,
                all(out),
                &mut scratch,
            );
            for y in 30..34 {
                for x in 0..96 {
                    src[(y * 96 + x) * 4] ^= 0xFF;
                }
            }
            let rows = affected_rows(30..34, view, out);
            crop_scale_rgba_smooth(&src, src_size, view, &mut partial, out, rows, &mut scratch);
            let mut full = vec![0u8; partial.len()];
            crop_scale_rgba_smooth(&src, src_size, view, &mut full, out, all(out), &mut scratch);
            assert_eq!(partial, full, "{view:?} -> {out:?}");
        }
    }
}
