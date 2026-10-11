//! Screen capture, cursor included.
//!
//! On Windows two backends sit behind [`ScreenCapture`]:
//!
//! - **DXGI desktop duplication** when the area lies on one monitor: the
//!   GPU copies the screen, and Windows says when nothing changed, so an
//!   idle screen costs almost nothing.
//! - **GDI** (`BitBlt`) for areas spanning monitors, or whenever
//!   duplication isn't available (some remote sessions and drivers). Set
//!   `BV_CAPTURE=gdi` to force it.

use std::io;

#[cfg(windows)]
mod cursor;
#[cfg(windows)]
mod dxgi;
#[cfg(windows)]
mod gdi;

/// A rectangle of the virtual desktop, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Captures one area of the desktop repeatedly. Not `Send`: create it on
/// the thread that uses it.
pub struct ScreenCapture {
    #[cfg(windows)]
    backend: Backend,
}

#[cfg(windows)]
enum Backend {
    Dxgi(dxgi::Dxgi),
    Gdi(gdi::Gdi),
}

impl ScreenCapture {
    #[cfg(windows)]
    pub fn new(area: Area, draw_cursor: bool) -> io::Result<Self> {
        if area.width == 0 || area.height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty capture area",
            ));
        }
        let force_gdi = std::env::var("BV_CAPTURE").is_ok_and(|v| v.eq_ignore_ascii_case("gdi"));
        let backend = match (force_gdi, dxgi::Dxgi::new(area, draw_cursor)) {
            (false, Ok(d)) => Backend::Dxgi(d),
            _ => Backend::Gdi(gdi::Gdi::new(area, draw_cursor)?),
        };
        Ok(Self { backend })
    }

    #[cfg(not(windows))]
    pub fn new(_area: Area, _draw_cursor: bool) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native capture is only implemented on Windows; use --engine ffmpeg",
        ))
    }

    /// Which backend is in use: `"dxgi"` or `"gdi"`.
    pub fn backend(&self) -> &'static str {
        #[cfg(windows)]
        match self.backend {
            Backend::Dxgi(_) => "dxgi",
            Backend::Gdi(_) => "gdi",
        }
        #[cfg(not(windows))]
        "none"
    }

    /// The next frame: BGRA rows, top-down, alpha undefined. `None` means
    /// it's certainly identical to the previous one (nothing on screen or
    /// of the cursor changed), so the caller can skip comparing it.
    pub fn grab(&mut self) -> io::Result<Option<&[u8]>> {
        #[cfg(windows)]
        match &mut self.backend {
            Backend::Dxgi(d) => d.grab(),
            Backend::Gdi(g) => g.grab().map(Some),
        }
        #[cfg(not(windows))]
        unreachable!("no capture backend")
    }
}

/// Bounds of all monitors together.
#[cfg(windows)]
pub fn virtual_screen() -> Area {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        Area {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN) as u32,
        }
    }
}

#[cfg(not(windows))]
pub fn virtual_screen() -> Area {
    Area {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    }
}
