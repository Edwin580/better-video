//! Palette selection and pixel mapping for GIF output.
//!
//! Screen recordings are mostly flat UI colours, so the builder first tries
//! to keep every exact colour (lossless when a clip uses at most
//! [`MAX_COLORS`]). Past that it falls back to a weighted median cut over a
//! 15-bit histogram, and maps pixels through a lazily filled 18-bit lookup
//! table so each pixel costs one table read.

use std::collections::{HashMap, HashSet};

/// Palette size; index 255 is left free for GIF transparency.
pub const MAX_COLORS: usize = 255;
/// Palette index reserved for "unchanged since the previous frame".
pub const TRANSPARENT: u8 = 255;

const HIST_BITS: u32 = 5;
const LUT_BITS: u32 = 6;

#[derive(Clone, Copy, Default)]
struct Bin {
    /// Pixels counted (the weight).
    n: u64,
    /// Pixels summed into r/g/b (the first of each run), for the mean.
    s: u64,
    r: u64,
    g: u64,
    b: u64,
}

/// Collects colour statistics from RGBA frames.
pub struct PaletteBuilder {
    exact: Option<HashSet<u32>>,
    hist: Vec<Bin>,
}

impl Default for PaletteBuilder {
    fn default() -> Self {
        Self {
            exact: Some(HashSet::new()),
            hist: vec![Bin::default(); 1 << (3 * HIST_BITS)],
        }
    }
}

impl PaletteBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count the pixels of one RGBA frame.
    pub fn add(&mut self, rgba: &[u8]) {
        let mut last = u32::MAX;
        for px in rgba.as_chunks::<4>().0.iter() {
            let c = rgb(px);
            // Runs of one colour are common; counting them once per run
            // keeps the histogram cheap without skewing it much.
            if c == last {
                self.hist[hist_index(px)].n += 1;
                continue;
            }
            last = c;
            let bin = &mut self.hist[hist_index(px)];
            bin.n += 1;
            bin.s += 1;
            bin.r += px[0] as u64;
            bin.g += px[1] as u64;
            bin.b += px[2] as u64;
            if let Some(set) = &mut self.exact {
                set.insert(c);
                if set.len() > MAX_COLORS {
                    self.exact = None;
                }
            }
        }
    }

    pub fn build(self) -> Palette {
        match self.exact {
            Some(set) if !set.is_empty() => {
                let mut colors: Vec<u32> = set.into_iter().collect();
                colors.sort_unstable();
                Palette::exact(colors.iter().map(|&c| unpack(c)).collect())
            }
            _ => Palette::approximate(median_cut(&self.hist, MAX_COLORS)),
        }
    }
}

/// A palette of at most [`MAX_COLORS`] colours plus a pixel → index mapper.
pub struct Palette {
    colors: Vec<[u8; 3]>,
    lookup: Lookup,
}

enum Lookup {
    Exact(HashMap<u32, u8>),
    /// 0xFF = not computed yet (index 255 is never a real colour).
    Nearest(Vec<u8>),
}

impl Palette {
    fn exact(colors: Vec<[u8; 3]>) -> Self {
        let map = colors
            .iter()
            .enumerate()
            .map(|(i, &c)| (pack(c), i as u8))
            .collect();
        Self {
            colors,
            lookup: Lookup::Exact(map),
        }
    }

    fn approximate(colors: Vec<[u8; 3]>) -> Self {
        let colors = if colors.is_empty() {
            vec![[0, 0, 0]]
        } else {
            colors
        };
        Self {
            colors,
            lookup: Lookup::Nearest(vec![TRANSPARENT; 1 << (3 * LUT_BITS)]),
        }
    }

    pub fn colors(&self) -> &[[u8; 3]] {
        &self.colors
    }

    /// True when every colour in the clip is in the palette exactly.
    pub fn is_lossless(&self) -> bool {
        matches!(self.lookup, Lookup::Exact(_))
    }

    /// Map RGBA pixels to palette indices.
    pub fn map(&mut self, rgba: &[u8], out: &mut [u8]) {
        let (mut last, mut last_i) = (u32::MAX, 0u8);
        for (px, o) in rgba.as_chunks::<4>().0.iter().zip(out) {
            let c = rgb(px);
            if c != last {
                last = c;
                last_i = self.index(px);
            }
            *o = last_i;
        }
    }

    fn index(&mut self, px: &[u8]) -> u8 {
        match &mut self.lookup {
            // A colour missing from an exact palette can't happen when the
            // same frames built it; fall back to black rather than panic.
            Lookup::Exact(map) => map.get(&rgb(px)).copied().unwrap_or(0),
            Lookup::Nearest(lut) => {
                let s = 8 - LUT_BITS;
                let key = ((px[0] as usize >> s) << (2 * LUT_BITS))
                    | ((px[1] as usize >> s) << LUT_BITS)
                    | (px[2] as usize >> s);
                if lut[key] == TRANSPARENT {
                    // Search from the centre of the LUT cell.
                    let half = 1 << (s - 1);
                    let c = [
                        (px[0] >> s << s) + half,
                        (px[1] >> s << s) + half,
                        (px[2] >> s << s) + half,
                    ];
                    lut[key] = nearest(&self.colors, c);
                }
                lut[key]
            }
        }
    }
}

fn nearest(colors: &[[u8; 3]], c: [u8; 3]) -> u8 {
    let mut best = (u32::MAX, 0);
    for (i, p) in colors.iter().enumerate() {
        let d = distance(*p, c);
        if d < best.0 {
            best = (d, i);
        }
    }
    best.1 as u8
}

/// Squared distance, weighted roughly by how sensitive eyes are to each
/// channel.
fn distance(a: [u8; 3], b: [u8; 3]) -> u32 {
    let d = |i: usize| (a[i] as i32 - b[i] as i32).pow(2) as u32;
    2 * d(0) + 4 * d(1) + 3 * d(2)
}

fn rgb(px: &[u8]) -> u32 {
    (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32
}

fn pack(c: [u8; 3]) -> u32 {
    rgb(&c)
}

fn unpack(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

fn hist_index(px: &[u8]) -> usize {
    let s = 8 - HIST_BITS;
    ((px[0] as usize >> s) << (2 * HIST_BITS))
        | ((px[1] as usize >> s) << HIST_BITS)
        | (px[2] as usize >> s)
}

/// A histogram bin reduced to its mean colour and pixel count.
#[derive(Clone, Copy)]
struct Entry {
    c: [f64; 3],
    n: f64,
}

/// Weighted median cut: repeatedly split the box with the largest squared
/// error along its widest channel, at the pixel-count median.
fn median_cut(hist: &[Bin], max: usize) -> Vec<[u8; 3]> {
    let entries: Vec<Entry> = hist
        .iter()
        .filter(|b| b.n > 0)
        .map(|b| {
            let s = b.s.max(1) as f64;
            Entry {
                c: [b.r as f64 / s, b.g as f64 / s, b.b as f64 / s],
                n: b.n as f64,
            }
        })
        .collect();
    if entries.is_empty() {
        return Vec::new();
    }

    let mut boxes: Vec<Vec<Entry>> = vec![entries];
    while boxes.len() < max {
        let Some((i, _)) = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| (i, sse(b)))
            .filter(|(_, e)| *e > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1))
        else {
            break;
        };
        let mut b = boxes.swap_remove(i);
        let ch = widest_channel(&b);
        b.sort_by(|x, y| x.c[ch].total_cmp(&y.c[ch]));
        let half = b.iter().map(|e| e.n).sum::<f64>() / 2.0;
        let mut acc = 0.0;
        let cut = b
            .iter()
            .position(|e| {
                acc += e.n;
                acc >= half
            })
            .map_or(1, |j| (j + 1).clamp(1, b.len() - 1));
        let tail = b.split_off(cut);
        boxes.push(b);
        boxes.push(tail);
    }

    boxes
        .iter()
        .map(|b| {
            let n: f64 = b.iter().map(|e| e.n).sum();
            let m = |ch: usize| (b.iter().map(|e| e.c[ch] * e.n).sum::<f64>() / n).round() as u8;
            [m(0), m(1), m(2)]
        })
        .collect()
}

fn sse(b: &[Entry]) -> f64 {
    let n: f64 = b.iter().map(|e| e.n).sum();
    (0..3)
        .map(|ch| {
            let mean = b.iter().map(|e| e.c[ch] * e.n).sum::<f64>() / n;
            b.iter()
                .map(|e| e.n * (e.c[ch] - mean).powi(2))
                .sum::<f64>()
        })
        .sum()
}

fn widest_channel(b: &[Entry]) -> usize {
    (0..3)
        .max_by(|&x, &y| {
            let range = |ch: usize| {
                let (lo, hi) = b.iter().fold((f64::MAX, f64::MIN), |(lo, hi), e| {
                    (lo.min(e.c[ch]), hi.max(e.c[ch]))
                });
                hi - lo
            };
            range(x).total_cmp(&range(y))
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(colors: &[[u8; 3]]) -> Vec<u8> {
        colors
            .iter()
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect()
    }

    #[test]
    fn few_colours_are_exact() {
        let f = frame(&[[255, 255, 255], [30, 30, 30], [0, 120, 215], [30, 30, 30]]);
        let mut b = PaletteBuilder::new();
        b.add(&f);
        let mut p = b.build();
        assert!(p.is_lossless());
        assert_eq!(p.colors().len(), 3);
        let mut idx = vec![0; 4];
        p.map(&f, &mut idx);
        for (i, px) in idx.iter().zip(f.chunks(4)) {
            assert_eq!(p.colors()[*i as usize], [px[0], px[1], px[2]]);
        }
    }

    #[test]
    fn many_colours_are_approximated_closely() {
        // A smooth gradient with far more than 255 colours.
        let colors: Vec<[u8; 3]> = (0..4096)
            .map(|i| [(i % 64 * 4) as u8, (i / 64 * 4) as u8, 128])
            .collect();
        let f = frame(&colors);
        let mut b = PaletteBuilder::new();
        b.add(&f);
        let mut p = b.build();
        assert!(!p.is_lossless());
        assert!(p.colors().len() <= MAX_COLORS);
        let mut idx = vec![0; colors.len()];
        p.map(&f, &mut idx);
        let worst = idx
            .iter()
            .zip(&colors)
            .map(|(&i, &c)| {
                let q = p.colors()[i as usize];
                (0..3)
                    .map(|ch| (q[ch] as i32 - c[ch] as i32).abs())
                    .max()
                    .unwrap()
            })
            .max()
            .unwrap();
        assert!(worst <= 24, "worst channel error {worst}");
        assert!(idx.iter().all(|&i| i != TRANSPARENT));
    }
}
