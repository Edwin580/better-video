//! Finding a top-level window by title, so a recording can target "the
//! app" instead of screen coordinates (handy for scripts and AI agents).

use std::fmt;

use crate::record::CaptureRegion;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub title: String,
    /// Visible bounds (without the invisible resize borders), in
    /// virtual-desktop pixels.
    pub region: CaptureRegion,
}

impl fmt::Display for Window {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}  (--region {})", self.title, self.region)
    }
}

/// The front-most window whose title contains `query` (case-insensitive).
pub fn find(query: &str) -> Result<Window, String> {
    let q = query.to_lowercase();
    let list = windows();
    list.iter()
        .find(|w| w.title.to_lowercase().contains(&q))
        .cloned()
        .ok_or_else(|| {
            if list.is_empty() {
                "finding windows by title is only supported on Windows so far".to_owned()
            } else {
                format!("no visible window with {query:?} in its title (see `bv windows`)")
            }
        })
}

#[cfg(windows)]
pub use win::windows;

/// Visible top-level windows, front to back.
#[cfg(not(windows))]
pub fn windows() -> Vec<Window> {
    Vec::new()
}

#[cfg(windows)]
mod win {
    use std::mem;

    use windows_sys::{
        core::BOOL,
        Win32::{
            Foundation::{HWND, LPARAM, RECT},
            Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS},
            UI::WindowsAndMessaging::{
                EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsIconic,
                IsWindowVisible,
            },
        },
    };

    use super::Window;
    use crate::record::CaptureRegion;

    /// Visible, non-minimized top-level windows with a title, in Z order
    /// (front first).
    pub fn windows() -> Vec<Window> {
        unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
            let out = &mut *(data as *mut Vec<Window>);
            if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                return 1;
            }
            // Cloaked windows (other virtual desktops, suspended UWP apps)
            // report as visible but aren't on screen.
            let mut cloaked = 0u32;
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED as u32,
                &mut cloaked as *mut _ as *mut _,
                mem::size_of::<u32>() as u32,
            );
            if cloaked != 0 {
                return 1;
            }
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return 1;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            let mut r: RECT = mem::zeroed();
            let dwm = DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                &mut r as *mut _ as *mut _,
                mem::size_of::<RECT>() as u32,
            );
            if dwm != 0 && GetWindowRect(hwnd, &mut r) == 0 {
                return 1;
            }
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            if w > 1 && h > 1 {
                out.push(Window {
                    title,
                    region: CaptureRegion {
                        x: r.left,
                        y: r.top,
                        width: w as u32,
                        height: h as u32,
                    },
                });
            }
            1
        }

        let mut list: Vec<Window> = Vec::new();
        unsafe { EnumWindows(Some(collect), &mut list as *mut _ as LPARAM) };
        list
    }
}
