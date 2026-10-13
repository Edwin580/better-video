//! Background thread that samples the global cursor position.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use bv_core::{CaptureRegion, CursorTrack};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(8); // ~120 Hz

pub struct CursorTracker {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<CursorTrack>,
}

impl CursorTracker {
    /// Start sampling. Times are relative to `origin`; positions relative
    /// to the region's top-left. Returns `None` when cursor tracking isn't
    /// available (feature disabled, or no display connection).
    pub fn start(origin: Instant, region: Option<CaptureRegion>) -> Option<Self> {
        let (ox, oy) = region.map_or((0.0, 0.0), |r| (r.x as f64, r.y as f64));
        let stop = Arc::new(AtomicBool::new(false));
        let poll = backend::poller()?;
        let flag = stop.clone();
        let handle = thread::spawn(move || {
            let mut track = CursorTrack::default();
            while !flag.load(Ordering::Relaxed) {
                let (x, y, pressed) = poll();
                track.push(bv_core::CursorSample {
                    t: origin.elapsed().as_secs_f64(),
                    x: x as f64 - ox,
                    y: y as f64 - oy,
                    pressed,
                });
                thread::sleep(SAMPLE_INTERVAL);
            }
            track
        });
        Some(Self { stop, handle })
    }

    /// Whether cursor tracking works here.
    pub fn available() -> bool {
        backend::poller().is_some()
    }

    pub fn finish(self) -> CursorTrack {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap_or_default()
    }
}

type Poller = Box<dyn Fn() -> (i32, i32, bool) + Send>;

#[cfg(feature = "input")]
mod backend {
    use device_query::{DeviceQuery, DeviceState};

    pub fn poller() -> Option<super::Poller> {
        // Probe on this thread so we can report unavailability up front.
        DeviceState::checked_new()?;
        Some(Box::new(|| {
            thread_local!(static STATE: DeviceState = DeviceState::new());
            STATE.with(|s| {
                let m = s.get_mouse();
                (m.coords.0, m.coords.1, m.button_pressed.iter().any(|&b| b))
            })
        }))
    }
}

#[cfg(not(feature = "input"))]
mod backend {
    pub fn poller() -> Option<super::Poller> {
        None
    }
}
