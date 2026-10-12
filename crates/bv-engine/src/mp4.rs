//! MP4 (H.264) with Windows' built-in Media Foundation encoder, so MP4
//! export needs no ffmpeg either.
//!
//! Windows "N" editions ship without Media Foundation unless the Media
//! Feature Pack is installed; [`Mp4Encoder::create`] fails there and the
//! caller can fall back to ffmpeg.

#[cfg(not(windows))]
use std::{io, path::Path};

#[cfg(windows)]
pub use win::Mp4Encoder;

#[cfg(not(windows))]
pub struct Mp4Encoder;

#[cfg(not(windows))]
impl Mp4Encoder {
    pub fn create(_path: &Path, _width: u32, _height: u32, _fps: u32) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "built-in MP4 encoding is Windows-only",
        ))
    }

    pub fn push(&mut self, _rgba: &[u8]) -> io::Result<()> {
        unreachable!()
    }

    pub fn finish(self) -> io::Result<()> {
        unreachable!()
    }
}

/// Average bit rate for screen content: about 0.04 bits per pixel per
/// frame, at least 300 kbit/s. Screen recordings are mostly still, so the
/// encoder rarely needs it all.
pub fn bitrate(width: u32, height: u32, fps: u32) -> u32 {
    let bits = width as f64 * height as f64 * fps as f64 * 0.04;
    bits.clamp(300_000.0, 20_000_000.0) as u32
}

#[cfg(windows)]
mod win {
    use std::{io, path::Path};

    use windows::{
        core::HSTRING,
        Win32::Media::MediaFoundation::{
            IMFAttributes, IMFMediaType, IMFSinkWriter, MFCreateAttributes, MFCreateMediaType,
            MFCreateMemoryBuffer, MFCreateSample, MFCreateSinkWriterFromURL, MFMediaType_Video,
            MFShutdown, MFStartup, MFVideoFormat_H264, MFVideoFormat_RGB32,
            MFVideoInterlace_Progressive, MFSTARTUP_FULL, MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE,
            MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
            MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
            MF_SINK_WRITER_DISABLE_THROTTLING, MF_VERSION,
        },
    };

    use super::bitrate;

    fn err(e: windows::core::Error) -> io::Error {
        io::Error::other(format!("Media Foundation: {}", e.message()))
    }

    /// Writes RGBA frames at a fixed frame rate to an `.mp4`.
    pub struct Mp4Encoder {
        writer: IMFSinkWriter,
        stream: u32,
        width: u32,
        height: u32,
        /// Frame duration and the next frame's start, in 100 ns units.
        duration: i64,
        time: i64,
    }

    /// Two 32-bit values packed the way Media Foundation's size and ratio
    /// attributes want them.
    fn pair(hi: u32, lo: u32) -> u64 {
        (hi as u64) << 32 | lo as u64
    }

    unsafe fn video_type(width: u32, height: u32, fps: u32) -> io::Result<IMFMediaType> {
        let t = MFCreateMediaType().map_err(err)?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(err)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(err)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pair(width, height))
            .map_err(err)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pair(fps, 1)).map_err(err)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pair(1, 1))
            .map_err(err)?;
        Ok(t)
    }

    impl Mp4Encoder {
        /// `width` and `height` must be even (H.264 with 4:2:0 chroma).
        pub fn create(path: &Path, width: u32, height: u32, fps: u32) -> io::Result<Self> {
            if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "MP4 needs an even width and height",
                ));
            }
            let fps = fps.max(1);
            unsafe {
                MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(err)?;
                let started = (|| {
                    let mut attrs: Option<IMFAttributes> = None;
                    MFCreateAttributes(&mut attrs, 2).map_err(err)?;
                    let attrs = attrs.ok_or_else(|| io::Error::other("no attributes"))?;
                    attrs
                        .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                        .map_err(err)?;
                    attrs
                        .SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)
                        .map_err(err)?;
                    let url = HSTRING::from(path.as_os_str());
                    let writer = MFCreateSinkWriterFromURL(&url, None, &attrs).map_err(err)?;

                    let out = video_type(width, height, fps)?;
                    out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                        .map_err(err)?;
                    out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate(width, height, fps))
                        .map_err(err)?;
                    let stream = writer.AddStream(&out).map_err(err)?;

                    let input = video_type(width, height, fps)?;
                    input
                        .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
                        .map_err(err)?;
                    // Positive stride: rows are top-down, like ours.
                    input
                        .SetUINT32(&MF_MT_DEFAULT_STRIDE, width * 4)
                        .map_err(err)?;
                    writer
                        .SetInputMediaType(stream, &input, None)
                        .map_err(err)?;
                    writer.BeginWriting().map_err(err)?;
                    Ok(Self {
                        writer,
                        stream,
                        width,
                        height,
                        duration: 10_000_000 / fps as i64,
                        time: 0,
                    })
                })();
                if started.is_err() {
                    let _ = MFShutdown();
                }
                started
            }
        }

        /// Add one RGBA frame (shown for one frame duration).
        pub fn push(&mut self, rgba: &[u8]) -> io::Result<()> {
            let len = self.width as usize * self.height as usize * 4;
            assert_eq!(rgba.len(), len, "frame size mismatch");
            unsafe {
                let buffer = MFCreateMemoryBuffer(len as u32).map_err(err)?;
                let mut data = std::ptr::null_mut();
                buffer.Lock(&mut data, None, None).map_err(err)?;
                let dst = std::slice::from_raw_parts_mut(data, len);
                // RGB32 is BGRX in memory.
                for (d, s) in dst
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(rgba.as_chunks::<4>().0)
                {
                    *d = [s[2], s[1], s[0], 255];
                }
                buffer.Unlock().map_err(err)?;
                buffer.SetCurrentLength(len as u32).map_err(err)?;
                let sample = MFCreateSample().map_err(err)?;
                sample.AddBuffer(&buffer).map_err(err)?;
                sample.SetSampleTime(self.time).map_err(err)?;
                sample.SetSampleDuration(self.duration).map_err(err)?;
                self.writer.WriteSample(self.stream, &sample).map_err(err)?;
            }
            self.time += self.duration;
            Ok(())
        }

        /// Flush and close the file.
        pub fn finish(self) -> io::Result<()> {
            unsafe {
                let done = self.writer.Finalize().map_err(err);
                drop(self.writer);
                let _ = MFShutdown();
                done
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn writes_a_playable_mp4() {
        let path = std::env::temp_dir().join(format!("bv-mp4-test-{}.mp4", std::process::id()));
        let (w, h) = (320u32, 180u32);
        let mut enc = Mp4Encoder::create(&path, w, h, 30).unwrap();
        let mut frame = vec![255u8; (w * h * 4) as usize];
        for i in 0..60u32 {
            // A dark box moving across a white frame.
            frame.fill(255);
            for y in 60..120 {
                for x in (i * 4)..(i * 4 + 40) {
                    let at = ((y * w + x.min(w - 1)) * 4) as usize;
                    frame[at..at + 3].copy_from_slice(&[20, 40, 200]);
                }
            }
            enc.push(&frame).unwrap();
        }
        enc.finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 1000, "{} bytes", bytes.len());
        assert_eq!(&bytes[4..8], b"ftyp", "an ISO media file");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_odd_sizes() {
        let path = std::env::temp_dir().join("bv-mp4-odd.mp4");
        assert!(Mp4Encoder::create(&path, 321, 180, 30).is_err());
    }
}
