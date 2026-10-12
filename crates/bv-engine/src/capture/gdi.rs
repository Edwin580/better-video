//! GDI capture: one `BitBlt` from the desktop into a DIB section we own,
//! with the cursor drawn on top. Works for any area, including ones that
//! span monitors.

use std::{io, mem, ptr};

use windows_sys::Win32::{
    Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP,
        HDC, HGDIOBJ, SRCCOPY,
    },
    UI::WindowsAndMessaging::{
        DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, HCURSOR,
        ICONINFO,
    },
};

use super::Area;

pub struct Gdi {
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

impl Gdi {
    pub fn new(area: Area, draw_cursor: bool) -> io::Result<Self> {
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

    /// Grab the area. Returns the capture buffer itself: BGRA rows,
    /// top-down, alpha undefined. No copy or conversion happens here;
    /// the writer only touches the tiles that changed.
    pub fn grab(&mut self) -> io::Result<&[u8]> {
        let a = self.area;
        let len = a.width as usize * a.height as usize * 4;
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
            Ok(std::slice::from_raw_parts(self.bits, len))
        }
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

impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.mem);
            ReleaseDC(ptr::null_mut(), self.screen);
        }
    }
}
