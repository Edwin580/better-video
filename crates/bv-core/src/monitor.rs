//! Connected displays, so a recording can target one screen instead of the
//! whole desktop.
//!
//! Coordinates are physical pixels in the virtual-desktop space: the primary
//! monitor's top-left is (0, 0) and monitors to its left or above have
//! negative offsets. Call [`set_dpi_aware`] once at startup so the OS reports
//! physical pixels (and cursor positions match) on scaled displays.

use std::{fmt, str::FromStr};

use crate::record::CaptureRegion;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// OS device name, e.g. `\\.\DISPLAY1`.
    pub name: String,
    pub region: CaptureRegion,
    pub primary: bool,
    /// UI scale in percent (100, 125, 150, …).
    pub scale: u32,
}

/// Which screen(s) to record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorChoice {
    /// The whole virtual desktop, every monitor side by side.
    All,
    Primary,
    /// 1-based, in the order [`monitors`] lists them.
    Index(usize),
}

impl FromStr for MonitorChoice {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "primary" | "main" => Ok(Self::Primary),
            n => match n.parse::<usize>() {
                Ok(i) if i >= 1 => Ok(Self::Index(i)),
                _ => Err(format!(
                    "invalid monitor {s:?}, expected a number (see `bv monitors`), primary or all"
                )),
            },
        }
    }
}

impl MonitorChoice {
    /// The region to capture; `None` means the whole desktop.
    pub fn region(self, monitors: &[Monitor]) -> Result<Option<CaptureRegion>, String> {
        match self {
            Self::All => Ok(None),
            Self::Primary => Ok(monitors.iter().find(|m| m.primary).map(|m| m.region)),
            Self::Index(i) => monitors.get(i - 1).map(|m| Some(m.region)).ok_or_else(|| {
                format!(
                    "monitor {i} not found ({} connected; see `bv monitors`)",
                    monitors.len()
                )
            }),
        }
    }
}

impl fmt::Display for Monitor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let r = self.region;
        write!(f, "{}x{} at ({}, {})", r.width, r.height, r.x, r.y)?;
        if self.scale != 100 {
            write!(f, ", {}% scale", self.scale)?;
        }
        if self.primary {
            write!(f, ", primary")?;
        }
        Ok(())
    }
}

/// Bounds of all monitors together (Windows; empty elsewhere).
pub fn virtual_screen() -> CaptureRegion {
    let a = bv_engine::capture::virtual_screen();
    CaptureRegion {
        x: a.x,
        y: a.y,
        width: a.width,
        height: a.height,
    }
}

#[cfg(windows)]
pub use win::{monitors, set_dpi_aware};

#[cfg(not(windows))]
/// Monitor enumeration isn't implemented on this platform yet.
pub fn monitors() -> Vec<Monitor> {
    Vec::new()
}

#[cfg(not(windows))]
pub fn set_dpi_aware() {}

#[cfg(windows)]
mod win {
    use std::{mem, ptr};

    use windows_sys::{
        core::BOOL,
        Win32::{
            Foundation::{LPARAM, RECT},
            Graphics::Gdi::{
                EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
            },
            UI::HiDpi::{
                GetDpiForMonitor, SetProcessDpiAwarenessContext,
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
            },
        },
    };

    use super::Monitor;
    use crate::record::CaptureRegion;

    const MONITORINFOF_PRIMARY: u32 = 1;

    /// Opt in to physical-pixel coordinates. Harmless if already set (e.g.
    /// by a manifest or an earlier call).
    pub fn set_dpi_aware() {
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }

    /// Connected monitors, left to right then top to bottom.
    pub fn monitors() -> Vec<Monitor> {
        unsafe extern "system" fn collect(
            hmon: HMONITOR,
            _: HDC,
            _: *mut RECT,
            data: LPARAM,
        ) -> BOOL {
            let out = &mut *(data as *mut Vec<Monitor>);
            let mut info: MONITORINFOEXW = mem::zeroed();
            info.monitorInfo.cbSize = mem::size_of::<MONITORINFOEXW>() as u32;
            if GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO) != 0 {
                let r = info.monitorInfo.rcMonitor;
                let (mut dx, mut dy) = (96, 96);
                GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
                let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(32);
                out.push(Monitor {
                    name: String::from_utf16_lossy(&info.szDevice[..len]),
                    region: CaptureRegion {
                        x: r.left,
                        y: r.top,
                        width: (r.right - r.left) as u32,
                        height: (r.bottom - r.top) as u32,
                    },
                    primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
                    scale: dx * 100 / 96,
                });
            }
            1
        }

        let mut list: Vec<Monitor> = Vec::new();
        unsafe {
            EnumDisplayMonitors(
                ptr::null_mut(),
                ptr::null(),
                Some(collect),
                &mut list as *mut _ as LPARAM,
            );
        }
        list.sort_by_key(|m| (m.region.x, m.region.y));
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(x: i32, primary: bool) -> Monitor {
        Monitor {
            name: String::new(),
            region: CaptureRegion {
                x,
                y: 0,
                width: 1920,
                height: 1080,
            },
            primary,
            scale: 100,
        }
    }

    #[test]
    fn picks_monitor_regions() {
        let list = [mon(-1920, false), mon(0, true)];
        assert_eq!("all".parse(), Ok(MonitorChoice::All));
        assert_eq!(MonitorChoice::All.region(&list), Ok(None));
        let primary = MonitorChoice::Primary.region(&list).unwrap().unwrap();
        assert_eq!(primary.x, 0);
        let left = "1".parse::<MonitorChoice>().unwrap().region(&list);
        assert_eq!(left.unwrap().unwrap().x, -1920);
        assert!(MonitorChoice::Index(3).region(&list).is_err());
        assert!("0".parse::<MonitorChoice>().is_err());
    }
}
