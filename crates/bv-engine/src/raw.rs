//! `.bvraw`: the native engine's intermediate recording.
//!
//! ```text
//! header:  "BVRAW\0\0\x01"  width:u32  height:u32
//! record:  0x01 t:f64 len:u32 lz4[len]   frame (only changed tiles)
//!          0x02 t:f64                    end of recording
//! ```
//!
//! A frame's decompressed payload is a list of `index:u32` + pixels for each
//! 32×32 tile that differs from the previous frame (edge tiles are clipped
//! to the image). Times are seconds since the recorder's origin. All
//! integers are little-endian; pixels are RGBA8.

use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};

const MAGIC: &[u8; 8] = b"BVRAW\0\0\x01";
const TILE: usize = 32;
const FRAME: u8 = 1;
const END: u8 = 2;

pub const EXTENSION: &str = "bvraw";

/// Whether `path` is a `.bvraw` recording (by its magic bytes).
pub fn is_raw(path: &Path) -> bool {
    let mut magic = [0u8; 8];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok_and(|()| &magic == MAGIC)
}

/// Tile `index`'s rectangle `(x, y, w, h)` in an image of `width × height`.
fn tile_rect(index: usize, width: usize, height: usize) -> (usize, usize, usize, usize) {
    let cols = width.div_ceil(TILE);
    let (x, y) = ((index % cols) * TILE, (index / cols) * TILE);
    (x, y, TILE.min(width - x), TILE.min(height - y))
}

fn tile_count(width: usize, height: usize) -> usize {
    width.div_ceil(TILE) * height.div_ceil(TILE)
}

pub struct RawWriter {
    out: BufWriter<File>,
    width: usize,
    height: usize,
    prev: Option<Vec<u8>>,
    payload: Vec<u8>,
    frames: u64,
}

impl RawWriter {
    pub fn create(path: &Path, width: u32, height: u32) -> io::Result<Self> {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
        out.write_all(MAGIC)?;
        out.write_all(&width.to_le_bytes())?;
        out.write_all(&height.to_le_bytes())?;
        Ok(Self {
            out,
            width: width as usize,
            height: height as usize,
            prev: None,
            payload: Vec::new(),
            frames: 0,
        })
    }

    /// Frames stored so far (unchanged frames aren't stored).
    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Store `rgba` captured at `t` seconds. Returns false (and writes
    /// nothing) when it's identical to the previous frame.
    pub fn push(&mut self, t: f64, rgba: &[u8]) -> io::Result<bool> {
        let (w, h) = (self.width, self.height);
        assert_eq!(rgba.len(), w * h * 4, "frame size mismatch");
        self.payload.clear();
        for i in 0..tile_count(w, h) {
            let (x, y, tw, th) = tile_rect(i, w, h);
            let rows = (y..y + th).map(|r| (r * w + x) * 4..(r * w + x + tw) * 4);
            let changed = match &self.prev {
                None => true,
                Some(prev) => rows.clone().any(|span| rgba[span.clone()] != prev[span]),
            };
            if changed {
                self.payload.extend_from_slice(&(i as u32).to_le_bytes());
                for span in rows {
                    self.payload.extend_from_slice(&rgba[span]);
                }
            }
        }
        if self.payload.is_empty() {
            return Ok(false);
        }
        match &mut self.prev {
            Some(prev) => prev.copy_from_slice(rgba),
            None => self.prev = Some(rgba.to_vec()),
        }
        let packed = lz4_flex::compress_prepend_size(&self.payload);
        self.out.write_all(&[FRAME])?;
        self.out.write_all(&t.to_le_bytes())?;
        self.out.write_all(&(packed.len() as u32).to_le_bytes())?;
        self.out.write_all(&packed)?;
        self.frames += 1;
        Ok(true)
    }

    /// Mark the end of the recording at `t` seconds and flush.
    pub fn finish(mut self, t: f64) -> io::Result<()> {
        self.out.write_all(&[END])?;
        self.out.write_all(&t.to_le_bytes())?;
        self.out.flush()
    }
}

/// Plays a `.bvraw` back frame by frame. Times are relative to the first
/// frame.
pub struct RawReader {
    r: BufReader<File>,
    width: u32,
    height: u32,
    canvas: Vec<u8>,
    first: f64,
    end: f64,
    /// Header of the next frame record, already read: (time, length).
    next: Option<(f64, u32)>,
    packed: Vec<u8>,
}

fn read_u8(r: &mut impl Read) -> io::Result<u8> {
    let mut b = [0; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_f64(r: &mut impl Read) -> io::Result<f64> {
    let mut b = [0; 8];
    r.read_exact(&mut b)?;
    Ok(f64::from_le_bytes(b))
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

impl RawReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut r = BufReader::with_capacity(1 << 20, File::open(path)?);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("not a .bvraw recording"));
        }
        let (width, height) = (read_u32(&mut r)?, read_u32(&mut r)?);
        if width == 0 || height == 0 || width > 1 << 15 || height > 1 << 15 {
            return Err(invalid("bad .bvraw dimensions"));
        }
        let body = r.stream_position()?;

        // Scan the record headers for the first and last timestamps. A
        // recording cut short (crash, power loss) has no end record; use the
        // last complete frame instead.
        let (mut first, mut end) = (None, 0.0);
        while let Ok(tag) = read_u8(&mut r) {
            let Ok(t) = read_f64(&mut r) else { break };
            match tag {
                FRAME => {
                    let Ok(len) = read_u32(&mut r) else { break };
                    if r.seek_relative(len as i64).is_err() {
                        break;
                    }
                    first.get_or_insert(t);
                    end = t;
                }
                END => {
                    end = t;
                    break;
                }
                _ => return Err(invalid("corrupt .bvraw record")),
            }
        }
        let first = first.ok_or_else(|| invalid("recording has no frames"))?;
        r.seek(SeekFrom::Start(body))?;
        let mut reader = Self {
            r,
            width,
            height,
            canvas: vec![0; width as usize * height as usize * 4],
            first,
            end: end.max(first),
            next: None,
            packed: Vec::new(),
        };
        reader.read_header()?;
        Ok(reader)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Seconds from the first frame to the end of the recording.
    pub fn duration(&self) -> f64 {
        self.end - self.first
    }

    /// Seconds between the recorder's origin and the first frame.
    pub fn start_offset(&self) -> f64 {
        self.first
    }

    /// The current frame (RGBA).
    pub fn frame(&self) -> &[u8] {
        &self.canvas
    }

    fn read_header(&mut self) -> io::Result<()> {
        self.next = None;
        let tag = match read_u8(&mut self.r) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };
        if tag == FRAME {
            let t = read_f64(&mut self.r)?;
            let len = read_u32(&mut self.r)?;
            self.next = Some((t - self.first, len));
        }
        Ok(())
    }

    /// Apply every frame up to time `t`. Returns whether the picture changed.
    pub fn advance_to(&mut self, t: f64) -> io::Result<bool> {
        let mut changed = false;
        while let Some((ft, len)) = self.next {
            if ft > t + 1e-9 {
                break;
            }
            self.packed.resize(len as usize, 0);
            self.r.read_exact(&mut self.packed)?;
            let payload = lz4_flex::decompress_size_prepended(&self.packed)
                .map_err(|_| invalid("corrupt .bvraw frame"))?;
            self.apply(&payload)?;
            changed = true;
            self.read_header()?;
        }
        Ok(changed)
    }

    fn apply(&mut self, mut payload: &[u8]) -> io::Result<()> {
        let (w, h) = (self.width as usize, self.height as usize);
        let tiles = tile_count(w, h);
        while !payload.is_empty() {
            let i = read_u32(&mut payload)? as usize;
            if i >= tiles {
                return Err(invalid("bad tile index"));
            }
            let (x, y, tw, th) = tile_rect(i, w, h);
            for r in y..y + th {
                let n = tw * 4;
                if payload.len() < n {
                    return Err(invalid("truncated tile"));
                }
                let at = (r * w + x) * 4;
                self.canvas[at..at + n].copy_from_slice(&payload[..n]);
                payload = &payload[n..];
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("bv-engine-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn round_trips_changed_tiles() {
        let (w, h) = (70u32, 40u32); // edge tiles are partial
        let path = temp("rt.bvraw");
        let mut a = vec![0u8; (w * h * 4) as usize];
        for (i, v) in a.iter_mut().enumerate() {
            *v = (i % 251) as u8;
        }
        let mut b = a.clone();
        b[((39 * w + 69) * 4) as usize] = 7; // bottom-right corner tile only

        let mut wr = RawWriter::create(&path, w, h).unwrap();
        assert!(wr.push(0.5, &a).unwrap());
        assert!(!wr.push(0.6, &a).unwrap(), "duplicate is skipped");
        assert!(wr.push(0.7, &b).unwrap());
        wr.finish(1.5).unwrap();
        assert!(is_raw(&path));

        let mut rd = RawReader::open(&path).unwrap();
        assert_eq!((rd.width(), rd.height()), (w, h));
        assert!((rd.duration() - 1.0).abs() < 1e-9);
        assert!((rd.start_offset() - 0.5).abs() < 1e-9);
        assert!(rd.advance_to(0.0).unwrap());
        assert_eq!(rd.frame(), &a[..]);
        assert!(!rd.advance_to(0.1).unwrap());
        assert!(rd.advance_to(0.2).unwrap());
        assert_eq!(rd.frame(), &b[..]);
        assert!(!rd.advance_to(10.0).unwrap());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_other_files() {
        let path = temp("not.bvraw");
        std::fs::write(&path, b"GIF89a....").unwrap();
        assert!(!is_raw(&path));
        assert!(RawReader::open(&path).is_err());
        std::fs::remove_file(path).ok();
    }
}
