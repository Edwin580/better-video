//! Per-frame crop + scale of raw RGBA frames.

use crate::geometry::{Rect, Size};

/// Sample `view` (a sub-rect of `src`) into `dst`, scaling with bilinear
/// filtering. Both buffers are tightly packed RGBA8.
///
/// Crop and scale happen in one pass so no intermediate frame is allocated.
pub fn crop_scale_rgba(src: &[u8], src_size: Size, view: Rect, dst: &mut [u8], dst_size: Size) {
    let (sw, sh) = (src_size.width as usize, src_size.height as usize);
    let (dw, dh) = (dst_size.width as usize, dst_size.height as usize);
    assert_eq!(src.len(), sw * sh * 4, "src buffer size mismatch");
    assert_eq!(dst.len(), dw * dh * 4, "dst buffer size mismatch");

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

    for (y, row) in dst.chunks_exact_mut(dw * 4).enumerate() {
        let fy = (view.y + (y as f64 + 0.5) * sy - 0.5).clamp(0.0, max_y);
        let y0 = fy as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let wy = ((fy - y0 as f64) * 256.0) as u32;
        let r0 = &src[y0 * sw * 4..(y0 + 1) * sw * 4];
        let r1 = &src[y1 * sw * 4..(y1 + 1) * sw * 4];

        for (px, &(x0, x1, wx)) in row.as_chunks_mut::<4>().0.iter_mut().zip(&xs) {
            for c in 0..4 {
                let top = r0[x0 + c] as u32 * (256 - wx) + r0[x1 + c] as u32 * wx;
                let bot = r1[x0 + c] as u32 * (256 - wx) + r1[x1 + c] as u32 * wx;
                px[c] = ((top * (256 - wy) + bot * wy + (1 << 15)) >> 16) as u8;
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
    scratch: &mut Vec<u8>,
) {
    let k = ((view.width / dst_size.width as f64).floor() as u32).clamp(1, 4);
    if k == 1 {
        if view == Rect::full(src_size) && dst_size == src_size {
            return dst.copy_from_slice(src);
        }
        return crop_scale_rgba(src, src_size, view, dst, dst_size);
    }
    let big = Size::new(dst_size.width * k, dst_size.height * k);
    // Whole frame at an exact integer ratio (1920→960, 1920→640): the
    // bilinear pass would be an identity copy, so average straight away.
    if view == Rect::full(src_size) && big == src_size {
        return box_reduce(src, src_size, k as usize, dst, dst_size);
    }
    scratch.resize(big.width as usize * big.height as usize * 4, 0);
    crop_scale_rgba(src, src_size, view, scratch, big);
    box_reduce(scratch, big, k as usize, dst, dst_size);
}

/// Average each `k×k` block of `src` into one pixel of `dst`.
fn box_reduce(src: &[u8], src_size: Size, k: usize, dst: &mut [u8], dst_size: Size) {
    let (sw, dw) = (src_size.width as usize, dst_size.width as usize);
    let area = (k * k) as u32;
    let mut sums = vec![0u32; dw * 4];
    for (y, row) in dst.chunks_exact_mut(dw * 4).enumerate() {
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

#[cfg(test)]
mod tests {
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
        crop_scale_rgba_smooth(
            &src,
            src_size,
            Rect::full(src_size),
            &mut dst,
            out,
            &mut scratch,
        );
        for px in dst.chunks(4) {
            assert!((px[0] as i32 - 128).abs() <= 1, "{dst:?}");
            assert_eq!(px[3], 255);
        }
    }

    use super::*;

    #[test]
    fn identity_copy() {
        let size = Size::new(3, 2);
        let src: Vec<u8> = (0..24).collect();
        let mut dst = vec![0; 24];
        crop_scale_rgba(&src, size, Rect::full(size), &mut dst, size);
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
        crop_scale_rgba(&src, size, view, &mut dst, out);
        assert!(dst.chunks(4).all(|p| p == [200, 200, 200, 255]), "{dst:?}");
    }
}
