//! Animated GIF writer tuned for screen recordings.
//!
//! Each frame only encodes the bounding box of what changed since the
//! previous one; inside that box, unchanged pixels become transparent so
//! LZW sees long runs. Frames identical to the previous one aren't written
//! at all: the previous frame's delay is extended instead.

use std::io::{self, Write};

use crate::{
    lzw,
    quant::{MAX_COLORS, TRANSPARENT},
};

pub struct GifEncoder<W: Write> {
    w: W,
    width: usize,
    height: usize,
    /// Indexed canvas as the viewer currently shows it.
    canvas: Vec<u8>,
    first: bool,
    pending: Option<Vec<u8>>,
    /// Seconds of animation written or pending, and centiseconds already
    /// committed to written delays; rounding error carries forward.
    clock: f64,
    committed_cs: u64,
    scratch: Vec<u8>,
}

impl<W: Write> GifEncoder<W> {
    /// Write the header and global palette (at most 255 colours).
    pub fn new(mut w: W, width: u16, height: u16, palette: &[[u8; 3]]) -> io::Result<Self> {
        assert!(palette.len() <= MAX_COLORS, "palette too large");
        w.write_all(b"GIF89a")?;
        w.write_all(&width.to_le_bytes())?;
        w.write_all(&height.to_le_bytes())?;
        // Global colour table of 256 entries (2^(7+1)), 8-bit resolution.
        w.write_all(&[0xF7, 0, 0])?;
        let mut table = [0u8; 256 * 3];
        for (i, c) in palette.iter().enumerate() {
            table[i * 3..i * 3 + 3].copy_from_slice(c);
        }
        w.write_all(&table)?;
        // Loop forever (NETSCAPE2.0 application extension).
        w.write_all(&[0x21, 0xFF, 11])?;
        w.write_all(b"NETSCAPE2.0")?;
        w.write_all(&[3, 1, 0, 0, 0])?;
        Ok(Self {
            w,
            width: width as usize,
            height: height as usize,
            canvas: vec![TRANSPARENT; width as usize * height as usize],
            first: true,
            pending: None,
            clock: 0.0,
            committed_cs: 0,
            scratch: Vec::new(),
        })
    }

    /// Add a frame of palette indices, shown for `secs`.
    pub fn push(&mut self, indices: &[u8], secs: f64) -> io::Result<()> {
        assert_eq!(
            indices.len(),
            self.width * self.height,
            "frame size mismatch"
        );
        let changed = if self.first {
            Some((0, 0, self.width, self.height))
        } else {
            changed_rect(&self.canvas, indices, self.width, self.height)
        };
        let Some((x, y, w, h)) = changed else {
            // Identical frame: the pending one just stays up longer.
            self.clock += secs;
            return Ok(());
        };
        self.flush_pending()?;

        let transparent = !self.first;
        self.scratch.clear();
        for row in y..y + h {
            let new = &indices[row * self.width + x..][..w];
            let old = &mut self.canvas[row * self.width + x..][..w];
            for (n, o) in new.iter().zip(old.iter_mut()) {
                if transparent && n == o {
                    self.scratch.push(TRANSPARENT);
                } else {
                    self.scratch.push(*n);
                    *o = *n;
                }
            }
        }
        self.first = false;

        let mut frame = Vec::with_capacity(self.scratch.len() / 2 + 32);
        // Graphic control extension; delay is filled in by flush_pending.
        let packed = (1 << 2) | transparent as u8; // disposal: leave in place
        frame.extend_from_slice(&[0x21, 0xF9, 4, packed, 0, 0, TRANSPARENT, 0]);
        frame.push(0x2C);
        for v in [x, y, w, h] {
            frame.extend_from_slice(&(v as u16).to_le_bytes());
        }
        frame.push(0); // no local colour table, not interlaced
        lzw::encode(&self.scratch, 8, &mut frame);
        self.pending = Some(frame);
        self.clock += secs;
        Ok(())
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        if let Some(mut frame) = self.pending.take() {
            let end_cs = (self.clock * 100.0).round() as u64;
            // Browsers treat delays under 2 cs as 10 cs; never go below 2.
            let delay = end_cs
                .saturating_sub(self.committed_cs)
                .clamp(2, u16::MAX as u64);
            self.committed_cs += delay;
            frame[4..6].copy_from_slice(&(delay as u16).to_le_bytes());
            self.w.write_all(&frame)?;
        }
        Ok(())
    }

    /// Write the last frame and the trailer.
    pub fn finish(mut self) -> io::Result<W> {
        self.flush_pending()?;
        self.w.write_all(&[0x3B])?;
        self.w.flush()?;
        Ok(self.w)
    }
}

/// Bounding box `(x, y, w, h)` of the pixels that differ, if any.
fn changed_rect(
    a: &[u8],
    b: &[u8],
    width: usize,
    height: usize,
) -> Option<(usize, usize, usize, usize)> {
    let rows = |r: usize| (&a[r * width..][..width], &b[r * width..][..width]);
    let top = (0..height).find(|&r| rows(r).0 != rows(r).1)?;
    let bottom = (top..height).rev().find(|&r| rows(r).0 != rows(r).1)?;
    let (mut left, mut right) = (width, 0);
    for r in top..=bottom {
        let (ra, rb) = rows(r);
        if let Some(l) = ra.iter().zip(rb).position(|(p, q)| p != q) {
            left = left.min(l);
            let rr = ra.iter().zip(rb).rposition(|(p, q)| p != q).unwrap_or(l);
            right = right.max(rr);
        }
    }
    Some((left, top, right - left + 1, bottom - top + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lzw::tests::decode;

    /// Minimal GIF reader: returns (delay_cs, x, y, w, h, indices) per frame.
    fn frames(gif: &[u8]) -> Vec<(u16, usize, usize, usize, usize, Vec<u8>)> {
        assert_eq!(&gif[..6], b"GIF89a");
        let mut i = 13 + 256 * 3;
        let mut out = Vec::new();
        let mut delay = 0;
        loop {
            match gif[i] {
                0x21 => {
                    if gif[i + 1] == 0xF9 {
                        delay = u16::from_le_bytes([gif[i + 4], gif[i + 5]]);
                    }
                    i += 2;
                    while gif[i] != 0 {
                        i += gif[i] as usize + 1;
                    }
                    i += 1;
                }
                0x2C => {
                    let v = |o: usize| u16::from_le_bytes([gif[i + o], gif[i + o + 1]]) as usize;
                    let (x, y, w, h) = (v(1), v(3), v(5), v(7));
                    i += 10;
                    let start = i;
                    i += 1;
                    while gif[i] != 0 {
                        i += gif[i] as usize + 1;
                    }
                    i += 1;
                    out.push((delay, x, y, w, h, decode(&gif[start..i])));
                }
                0x3B => return out,
                b => panic!("unexpected block {b:#x}"),
            }
        }
    }

    #[test]
    fn encodes_deltas_and_merges_duplicates() {
        let palette = [[0, 0, 0], [255, 255, 255]];
        let mut enc = GifEncoder::new(Vec::new(), 4, 3, &palette).unwrap();
        let a = vec![0u8; 12];
        let mut b = a.clone();
        b[5] = 1; // (1, 1)
        b[6] = 1; // (2, 1)
        enc.push(&a, 0.1).unwrap();
        enc.push(&a, 0.1).unwrap();
        enc.push(&b, 0.1).unwrap();
        enc.push(&b, 0.05).unwrap();
        let gif = enc.finish().unwrap();
        let f = frames(&gif);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].0, 20, "two identical frames merge");
        assert_eq!((f[0].1, f[0].2, f[0].3, f[0].4), (0, 0, 4, 3));
        assert_eq!(f[0].5, a);
        assert_eq!(f[1].0, 15);
        assert_eq!((f[1].1, f[1].2, f[1].3, f[1].4), (1, 1, 2, 1));
        assert_eq!(f[1].5, [1, 1]);
    }

    #[test]
    fn unchanged_pixels_inside_rect_are_transparent() {
        let mut enc = GifEncoder::new(Vec::new(), 3, 1, &[[0; 3], [9; 3]]).unwrap();
        enc.push(&[0, 0, 0], 0.1).unwrap();
        enc.push(&[1, 0, 1], 0.1).unwrap();
        let f = frames(&enc.finish().unwrap());
        assert_eq!(f[1].5, [1, TRANSPARENT, 1]);
    }

    #[test]
    fn rounding_error_does_not_accumulate() {
        let mut enc = GifEncoder::new(Vec::new(), 1, 1, &[[0; 3], [9; 3]]).unwrap();
        for i in 0..30 {
            enc.push(&[(i % 2) as u8], 1.0 / 30.0).unwrap();
        }
        let total: u32 = frames(&enc.finish().unwrap())
            .iter()
            .map(|f| f.0 as u32)
            .sum();
        assert_eq!(total, 100);
    }
}
