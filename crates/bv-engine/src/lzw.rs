//! GIF-flavoured LZW compression (variable code width, LSB-first bit
//! packing, 255-byte sub-blocks).

const MAX_CODES: u16 = 4096;
/// Open-addressing table for (prefix code, next byte) → code. A power of
/// two comfortably larger than `MAX_CODES` keeps probe chains short.
const SLOTS: usize = 1 << 13;

/// Compress `indices` (each `< 1 << min_code_size`) as a GIF image data
/// block: the minimum code size byte, the sub-blocks, and the terminator.
pub fn encode(indices: &[u8], min_code_size: u8, out: &mut Vec<u8>) {
    let min = min_code_size.max(2);
    out.push(min);
    let mut bits = BitWriter::new(out);
    let clear = 1u16 << min;
    let eoi = clear + 1;

    let mut keys = vec![0u32; SLOTS];
    let mut codes = vec![0u16; SLOTS];
    let mut next = eoi + 1;
    let mut width = min + 1;
    bits.write(clear, width);

    let Some((&first, rest)) = indices.split_first() else {
        bits.write(eoi, width);
        bits.finish();
        return;
    };
    let mut prefix = first as u16;
    for &k in rest {
        // Stored keys are offset by one so zero means "empty slot".
        let key = ((prefix as u32) << 8 | k as u32) + 1;
        let mut slot = hash(key);
        loop {
            match keys[slot] {
                0 => break,
                kk if kk == key => break,
                _ => slot = (slot + 1) & (SLOTS - 1),
            }
        }
        if keys[slot] == key {
            prefix = codes[slot];
            continue;
        }
        bits.write(prefix, width);
        // Widen once the decoder's table (one entry behind ours) fills the
        // current width.
        if next >= 1 << width && width < 12 {
            width += 1;
        }
        if next < MAX_CODES {
            keys[slot] = key;
            codes[slot] = next;
            next += 1;
        } else {
            bits.write(clear, width);
            keys.fill(0);
            next = eoi + 1;
            width = min + 1;
        }
        prefix = k as u16;
    }
    bits.write(prefix, width);
    if next >= 1 << width && width < 12 {
        width += 1;
    }
    bits.write(eoi, width);
    bits.finish();
}

fn hash(key: u32) -> usize {
    (key.wrapping_mul(0x9E37_79B1) >> 19) as usize & (SLOTS - 1)
}

/// Packs codes LSB-first into GIF sub-blocks.
struct BitWriter<'a> {
    out: &'a mut Vec<u8>,
    acc: u32,
    n: u8,
    block: [u8; 255],
    len: usize,
}

impl<'a> BitWriter<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        Self {
            out,
            acc: 0,
            n: 0,
            block: [0; 255],
            len: 0,
        }
    }

    fn write(&mut self, code: u16, width: u8) {
        self.acc |= (code as u32) << self.n;
        self.n += width;
        while self.n >= 8 {
            self.byte(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    fn byte(&mut self, b: u8) {
        self.block[self.len] = b;
        self.len += 1;
        if self.len == 255 {
            self.flush_block();
        }
    }

    fn flush_block(&mut self) {
        if self.len > 0 {
            self.out.push(self.len as u8);
            self.out.extend_from_slice(&self.block[..self.len]);
            self.len = 0;
        }
    }

    fn finish(mut self) {
        if self.n > 0 {
            self.byte(self.acc as u8);
        }
        self.flush_block();
        self.out.push(0);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Reference GIF LZW decoder for round-trip tests.
    pub(crate) fn decode(data: &[u8]) -> Vec<u8> {
        let min = data[0];
        let mut bytes = Vec::new();
        let mut i = 1;
        while data[i] != 0 {
            let n = data[i] as usize;
            bytes.extend_from_slice(&data[i + 1..i + 1 + n]);
            i += n + 1;
        }
        let clear = 1u16 << min;
        let eoi = clear + 1;
        let mut table: Vec<Vec<u8>> = Vec::new();
        let reset = |t: &mut Vec<Vec<u8>>| {
            t.clear();
            t.extend((0..clear).map(|c| vec![c as u8]));
            t.push(vec![]);
            t.push(vec![]);
        };
        reset(&mut table);
        let mut width = min + 1;
        let (mut acc, mut n, mut pos) = (0u32, 0u8, 0usize);
        let mut prev: Option<Vec<u8>> = None;
        let mut out = Vec::new();
        loop {
            while n < width {
                acc |= (bytes[pos] as u32) << n;
                pos += 1;
                n += 8;
            }
            let code = (acc & ((1 << width) - 1)) as u16;
            acc >>= width;
            n -= width;
            if code == clear {
                reset(&mut table);
                width = min + 1;
                prev = None;
                continue;
            }
            if code == eoi {
                return out;
            }
            let entry = if (code as usize) < table.len() {
                table[code as usize].clone()
            } else {
                let p = prev.clone().expect("KwKwK needs a previous code");
                let mut e = p.clone();
                e.push(p[0]);
                e
            };
            if let Some(p) = prev {
                if table.len() < 4096 {
                    let mut e = p;
                    e.push(entry[0]);
                    table.push(e);
                }
            }
            if table.len() == 1 << width && width < 12 {
                width += 1;
            }
            out.extend_from_slice(&entry);
            prev = Some(entry);
        }
    }

    fn round_trip(input: &[u8], min: u8) {
        let mut enc = Vec::new();
        encode(input, min, &mut enc);
        assert_eq!(decode(&enc), input);
    }

    #[test]
    fn round_trips() {
        round_trip(&[], 8);
        round_trip(&[7], 8);
        round_trip(&[1, 1, 1, 1, 1, 1, 1, 1], 2);
        round_trip(&vec![42; 100_000], 8);
        // Pseudo-random data overflows the 4096-code table several times.
        let mut x = 12345u32;
        let noise: Vec<u8> = (0..200_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x % 255) as u8
            })
            .collect();
        round_trip(&noise, 8);
        let few: Vec<u8> = noise.iter().map(|v| v % 3).collect();
        round_trip(&few, 2);
    }

    #[test]
    fn compresses_repetition() {
        let mut enc = Vec::new();
        encode(&vec![9; 640 * 360], 8, &mut enc);
        assert!(enc.len() < 2_000, "{}", enc.len());
    }
}
