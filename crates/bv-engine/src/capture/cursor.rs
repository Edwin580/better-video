//! Drawing the mouse cursor into a captured frame.
//!
//! Cursors can invert what's under them (the text I-beam does), so they
//! are drawn by Windows itself with `DrawIconEx` onto the real pixels: the
//! patch under the cursor is copied into a small DIB, drawn on, and copied
//! back.

use std::{mem, ptr};

use windows_sys::Win32::{
    Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetObjectW, SelectObject,
        BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
    },
    UI::WindowsAndMessaging::{
        DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, HCURSOR,
        ICONINFO,
    },
};

use super::Area;

/// Largest cursor drawn (Windows' accessibility sizes go up to 256).
const MAX: i32 = 256;

/// Where the cursor is and what it looks like; equal states draw equally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorState {
    handle: isize,
    x: i32,
    y: i32,
}

/// Shape details cached per cursor handle.
#[derive(Clone, Copy)]
struct Shape {
    handle: HCURSOR,
    hot_x: i32,
    hot_y: i32,
    width: i32,
    height: i32,
}

pub struct CursorPainter {
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    shape: Option<Shape>,
}

impl CursorPainter {
    pub fn new() -> Option<Self> {
        unsafe {
            let dc = CreateCompatibleDC(ptr::null_mut());
            let mut info: BITMAPINFO = mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: MAX,
                biHeight: -MAX,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..mem::zeroed()
            };
            let mut bits = ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            let old = SelectObject(dc, bitmap);
            Some(Self {
                dc,
                bitmap,
                old,
                bits: bits as *mut u8,
                shape: None,
            })
        }
    }

    /// The current cursor, if visible.
    pub fn state() -> Option<CursorState> {
        let mut ci: CURSORINFO = unsafe { mem::zeroed() };
        ci.cbSize = mem::size_of::<CURSORINFO>() as u32;
        let ok = unsafe { GetCursorInfo(&mut ci) } != 0;
        (ok && ci.flags & CURSOR_SHOWING != 0).then_some(CursorState {
            handle: ci.hCursor as isize,
            x: ci.ptScreenPos.x,
            y: ci.ptScreenPos.y,
        })
    }

    fn shape(&mut self, handle: HCURSOR) -> Option<Shape> {
        if let Some(s) = self.shape.filter(|s| s.handle == handle) {
            return Some(s);
        }
        unsafe {
            let mut ii: ICONINFO = mem::zeroed();
            if GetIconInfo(handle, &mut ii) == 0 {
                return None;
            }
            // Colour cursors have a colour bitmap; monochrome ones stack
            // their AND and XOR masks in one bitmap twice as tall.
            let (src, halve) = if ii.hbmColor.is_null() {
                (ii.hbmMask, true)
            } else {
                (ii.hbmColor, false)
            };
            let mut bm: BITMAP = mem::zeroed();
            GetObjectW(
                src,
                mem::size_of::<BITMAP>() as i32,
                &mut bm as *mut _ as *mut _,
            );
            for b in [ii.hbmMask, ii.hbmColor] {
                if !b.is_null() {
                    DeleteObject(b);
                }
            }
            let s = Shape {
                handle,
                hot_x: ii.xHotspot as i32,
                hot_y: ii.yHotspot as i32,
                width: bm.bmWidth.min(MAX),
                height: (if halve { bm.bmHeight / 2 } else { bm.bmHeight }).min(MAX),
            };
            self.shape = Some(s);
            Some(s)
        }
    }

    /// Draw `cursor` into `frame` (BGRA rows of `area`). Returns the area it
    /// touched, as `(x, y, w, h)` in frame pixels, so it can be undone.
    pub fn paint(
        &mut self,
        frame: &mut [u8],
        area: Area,
        cursor: CursorState,
    ) -> Option<(usize, usize, usize, usize)> {
        let s = self.shape(cursor.handle as HCURSOR)?;
        // Cursor rectangle in frame coordinates, clipped to the frame.
        let left = cursor.x - s.hot_x - area.x;
        let top = cursor.y - s.hot_y - area.y;
        let (x0, y0) = (left.max(0), top.max(0));
        let x1 = (left + s.width).min(area.width as i32);
        let y1 = (top + s.height).min(area.height as i32);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let stride = area.width as usize * 4;
        // Patch origin inside the DIB: where (x0, y0) lands relative to the
        // cursor's own top-left.
        let (px, py) = ((x0 - left) as usize, (y0 - top) as usize);
        let dib = unsafe { std::slice::from_raw_parts_mut(self.bits, (MAX * MAX * 4) as usize) };
        let dib_stride = MAX as usize * 4;
        for row in 0..h {
            let f = (y0 as usize + row) * stride + x0 as usize * 4;
            let d = (py + row) * dib_stride + px * 4;
            dib[d..d + w * 4].copy_from_slice(&frame[f..f + w * 4]);
        }
        unsafe {
            DrawIconEx(self.dc, 0, 0, s.handle, 0, 0, 0, ptr::null_mut(), DI_NORMAL);
        }
        for row in 0..h {
            let f = (y0 as usize + row) * stride + x0 as usize * 4;
            let d = (py + row) * dib_stride + px * 4;
            frame[f..f + w * 4].copy_from_slice(&dib[d..d + w * 4]);
        }
        Some((x0 as usize, y0 as usize, w, h))
    }
}

impl Drop for CursorPainter {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

impl CursorState {
    pub fn x(&self) -> i32 {
        self.x
    }

    pub fn y(&self) -> i32 {
        self.y
    }
}
