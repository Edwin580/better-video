//! Capture → `.bvraw` on a background thread.

use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    capture::{Area, ScreenCapture},
    raw::RawWriter,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecorderStats {
    /// Seconds from the origin to the first captured frame.
    pub first_frame: f64,
    /// Frames captured, including ones identical to the previous frame.
    pub captured: u64,
    /// Frames that differed and were stored.
    pub stored: u64,
    /// Capture backend used ("dxgi" or "gdi").
    pub backend: &'static str,
    /// Seconds spent grabbing frames and storing them.
    pub grab_secs: f64,
    pub store_secs: f64,
}

pub struct Recorder {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<io::Result<RecorderStats>>,
}

impl Recorder {
    /// Start capturing `area` at `fps` into `path`. Timestamps are seconds
    /// since `origin`. Fails fast if the capture can't be set up.
    pub fn start(
        path: PathBuf,
        area: Area,
        fps: u32,
        draw_cursor: bool,
        origin: Instant,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let handle = thread::Builder::new()
            .name("bv-capture".into())
            .spawn(move || {
                // GDI objects belong to the thread that created them.
                let setup = ScreenCapture::new(area, draw_cursor)
                    .and_then(|c| Ok((c, RawWriter::create(&path, area.width, area.height)?)));
                let (mut capture, mut writer) = match setup {
                    Ok(v) => {
                        let _ = ready_tx.send(Ok(()));
                        v
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(io::Error::new(e.kind(), e.to_string())));
                        return Err(e);
                    }
                };
                run(&mut capture, &mut writer, fps, origin, &flag).and_then(|stats| {
                    writer.finish(origin.elapsed().as_secs_f64())?;
                    Ok(stats)
                })
            })?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { stop, handle }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(e)
            }
            Err(_) => Err(io::Error::other("capture thread exited during setup")),
        }
    }

    /// True if the capture thread has stopped on its own (an error).
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Stop capturing and finalize the file.
    pub fn stop(self) -> io::Result<RecorderStats> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle
            .join()
            .map_err(|_| io::Error::other("capture thread panicked"))?
    }
}

fn run(
    capture: &mut ScreenCapture,
    writer: &mut RawWriter,
    fps: u32,
    origin: Instant,
    stop: &AtomicBool,
) -> io::Result<RecorderStats> {
    let interval = Duration::from_secs_f64(1.0 / fps.clamp(1, 60) as f64);
    let mut stats = RecorderStats {
        first_frame: 0.0,
        captured: 0,
        stored: 0,
        backend: capture.backend(),
        grab_secs: 0.0,
        store_secs: 0.0,
    };
    let mut next = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let t = origin.elapsed().as_secs_f64();
        let started = Instant::now();
        let grabbed = capture.grab();
        stats.grab_secs += started.elapsed().as_secs_f64();
        let frame = match grabbed {
            Ok(frame) => frame,
            // The first grab failing means the capture can't work at all.
            Err(e) if stats.captured == 0 => return Err(e),
            // Later failures are usually transient (screen locked, UAC
            // prompt on the secure desktop): keep the last good frame on
            // screen and try again next tick rather than lose the clip.
            Err(_) => {
                wait(&mut next, interval);
                continue;
            }
        };
        if stats.captured == 0 {
            stats.first_frame = t;
        }
        stats.captured += 1;
        // `None`: the backend knows nothing changed, so skip comparing.
        if let Some(frame) = frame {
            let started = Instant::now();
            if writer.push(t, frame)? {
                stats.stored += 1;
            }
            stats.store_secs += started.elapsed().as_secs_f64();
        }
        wait(&mut next, interval);
    }
    Ok(stats)
}

/// Sleep until the next tick at a fixed cadence; if a grab overran, skip
/// ahead rather than burst.
fn wait(next: &mut Instant, interval: Duration) {
    *next += interval;
    let now = Instant::now();
    if *next > now {
        thread::sleep(*next - now);
    } else {
        *next = now;
    }
}
