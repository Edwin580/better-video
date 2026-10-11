//! Screen capture. Windows uses GDI: one `BitBlt` from the desktop into a
//! DIB section we own, plus the cursor drawn on top.

#[cfg(not(windows))]
use std::io;

/// A rectangle of the virtual desktop, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[cfg(windows)]
pub use win::{virtual_screen, ScreenCapture};

#[cfg(not(windows))]
pub struct ScreenCapture;

#[cfg(not(windows))]
impl ScreenCapture {
    pub fn new(_area: Area, _draw_cursor: bool) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native capture is only implemented on Windows; use --engine ffmpeg",
        ))
    }

    pub fn grab(&mut self, _rgba: &mut [u8]) -> io::Result<()> {
        unreachable!()
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

#[cfg(windows)]
mod win {
    use std::{io, mem, ptr};

    use windows_sys::Win32::{
        Graphics::Gdi::{
            BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
            SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS,
            HBITMAP, HDC, HGDIOBJ, SRCCOPY,
        },
        UI::WindowsAndMessaging::{
            DrawIconEx, GetCursorInfo, GetIconInfo, GetSystemMetrics, CURSORINFO, CURSOR_SHOWING,
            DI_NORMAL, HCURSOR, ICONINFO, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
            SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
        },
    };

    use super::Area;

    /// Bounds of all monitors together.
    pub fn virtual_screen() -> Area {
        unsafe {
            Area {
                x: GetSystemMetrics(SM_XVIRTUALSCREEN),
                y: GetSystemMetrics(SM_YVIRTUALSCREEN),
                width: GetSystemMetrics(SM_CXVIRTUALSCREEN) as u32,
                height: GetSystemMetrics(SM_CYVIRTUALSCREEN) as u32,
            }
        }
    }

    /// Captures one area of the desktop repeatedly. Not `Send`: create it on
    /// the thread that uses it.
    pub struct ScreenCapture {
        area: Area,
        draw_cursor: bool,
        screen: HDC,
        mem: HDC,
        bitmap: HBITMAP,
        old: HGDIOBJ,
        bits: *const u8,
        /// Hotspot of the last cursor drawn, keyed by its handle.
        hotspot: Option<(HCURSOR, i32, i32)>,
    }

    impl ScreenCapture {
        pub fn new(area: Area, draw_cursor: bool) -> io::Result<Self> {
            if area.width == 0 || area.height == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "empty capture area",
                ));
            }
            unsafe {
                let screen = GetDC(ptr::null_mut());
                if screen.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let mem = CreateCompatibleDC(screen);
                let mut info: BITMAPINFO = mem::zeroed();
                info.bmiHeader = BITMAPINFOHEADER {
                    biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: area.width as i32,
                    // Negative height: rows top-down, like every other buffer.
                    biHeight: -(area.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..mem::zeroed()
                };
                let mut bits = ptr::null_mut();
                let bitmap =
                    CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
                if bitmap.is_null() || bits.is_null() {
                    let err = io::Error::last_os_error();
                    DeleteDC(mem);
                    ReleaseDC(ptr::null_mut(), screen);
                    return Err(err);
                }
                let old = SelectObject(mem, bitmap);
                Ok(Self {
                    area,
                    draw_cursor,
                    screen,
                    mem,
                    bitmap,
                    old,
                    bits: bits as *const u8,
                    hotspot: None,
                })
            }
        }

        /// Grab the area into `rgba` (`width * height * 4` bytes).
        pub fn grab(&mut self, rgba: &mut [u8]) -> io::Result<()> {
            let a = self.area;
            let len = a.width as usize * a.height as usize * 4;
            assert_eq!(rgba.len(), len, "buffer size mismatch");
            unsafe {
                // CAPTUREBLT includes layered windows (menus, tooltips).
                let ok = BitBlt(
                    self.mem,
                    0,
                    0,
                    a.width as i32,
                    a.height as i32,
                    self.screen,
                    a.x,
                    a.y,
                    SRCCOPY | CAPTUREBLT,
                );
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                if self.draw_cursor {
                    self.draw_cursor();
                }
                let bgra = std::slice::from_raw_parts(self.bits, len);
                for (o, i) in rgba
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(bgra.as_chunks::<4>().0.iter())
                {
                    o.copy_from_slice(&[i[2], i[1], i[0], 255]);
                }
            }
            Ok(())
        }

        unsafe fn draw_cursor(&mut self) {
            let mut ci: CURSORINFO = mem::zeroed();
            ci.cbSize = mem::size_of::<CURSORINFO>() as u32;
            if GetCursorInfo(&mut ci) == 0 || ci.flags & CURSOR_SHOWING == 0 {
                return;
            }
            let (hx, hy) = match self.hotspot {
                Some((h, x, y)) if h == ci.hCursor => (x, y),
                _ => {
                    let mut ii: ICONINFO = mem::zeroed();
                    if GetIconInfo(ci.hCursor, &mut ii) == 0 {
                        return;
                    }
                    // GetIconInfo hands us copies of the bitmaps.
                    if !ii.hbmMask.is_null() {
                        DeleteObject(ii.hbmMask);
                    }
                    if !ii.hbmColor.is_null() {
                        DeleteObject(ii.hbmColor);
                    }
                    let (x, y) = (ii.xHotspot as i32, ii.yHotspot as i32);
                    self.hotspot = Some((ci.hCursor, x, y));
                    (x, y)
                }
            };
            DrawIconEx(
                self.mem,
                ci.ptScreenPos.x - hx - self.area.x,
                ci.ptScreenPos.y - hy - self.area.y,
                ci.hCursor,
                0,
                0,
                0,
                ptr::null_mut(),
                DI_NORMAL,
            );
        }
    }

    impl Drop for ScreenCapture {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.mem, self.old);
                DeleteObject(self.bitmap);
                DeleteDC(self.mem);
                ReleaseDC(ptr::null_mut(), self.screen);
            }
        }
    }
}
