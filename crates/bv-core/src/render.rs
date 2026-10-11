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

        for (px, &(x0, x1, wx)) in row.chunks_exact_mut(4).zip(&xs) {
            for c in 0..4 {
                let top = r0[x0 + c] as u32 * (256 - wx) + r0[x1 + c] as u32 * wx;
                let bot = r1[x0 + c] as u32 * (256 - wx) + r1[x1 + c] as u32 * wx;
                px[c] = ((top * (256 - wy) + bot * wy + (1 << 15)) >> 16) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
