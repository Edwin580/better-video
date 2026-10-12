//! A premultiplied-BGRA drawing surface for layered windows: a DIB section
//! we draw into directly, presented with `UpdateLayeredWindow`.
//!
//! Shapes are drawn from signed distance functions, so every edge is
//! anti-aliased, and shadows are just a wider falloff of the same shape.
//! The canvas remembers the area drawn since [`Canvas::take_ink`], so a
//! window can repaint (and re-upload) only the part that changed.

use std::{mem, ptr, sync::OnceLock};

use windows_sys::Win32::{
    Foundation::{HWND, POINT, RECT, SIZE},
    Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, GetDC,
        GetTextExtentPoint32W, GetTextFaceW, ReleaseDC, SelectObject, SetBkMode, SetTextColor,
        TextOutW, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION,
        CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS,
        HBITMAP, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
    },
    UI::WindowsAndMessaging::{UpdateLayeredWindowIndirect, ULW_ALPHA, UPDATELAYEREDWINDOWINFO},
};

use super::wide;

/// Integer rectangle in canvas pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn inset(&self, d: i32) -> Self {
        Self::new(self.x + d, self.y + d, self.w - 2 * d, self.h - 2 * d)
    }

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    pub fn union(&self, o: &Rect) -> Rect {
        let (x, y) = (self.x.min(o.x), self.y.min(o.y));
        Rect::new(
            x,
            y,
            self.right().max(o.right()) - x,
            self.bottom().max(o.bottom()) - y,
        )
    }
}

/// Premultiplied colour, `0xAARRGGBB` as the DIB stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        const fn p(c: u8, a: u8) -> u32 {
            (c as u32 * a as u32 + 127) / 255
        }
        Self((a as u32) << 24 | p(r, a) << 16 | p(g, a) << 8 | p(b, a))
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    /// Scale all channels (premultiplied) by `k` in 0..=1.
    fn scale(self, k: f32) -> u32 {
        let k = (k.clamp(0.0, 1.0) * 256.0) as u32;
        let c = self.0;
        let ch = |s: u32| ((((c >> s) & 0xFF) * k) >> 8) << s;
        ch(24) | ch(16) | ch(8) | ch(0)
    }

    /// Opaque colour as GDI's `COLORREF` (for text).
    fn colorref(self) -> u32 {
        let c = self.0;
        ((c >> 16) & 0xFF) | (c & 0xFF00) | ((c & 0xFF) << 16)
    }
}

/// Font weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Regular = 400,
    Semibold = 600,
}

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    fonts: Vec<(i32, Weight, HFONT)>,
    /// The DC's original font, restored before ours are deleted (GDI won't
    /// delete an object that is still selected).
    old_font: Option<HGDIOBJ>,
    /// Area drawn since the last `take_ink`.
    ink: Option<Rect>,
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Self {
        unsafe {
            let screen = GetDC(ptr::null_mut());
            let dc = CreateCompatibleDC(screen);
            ReleaseDC(ptr::null_mut(), screen);
            let mut info: BITMAPINFO = mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..mem::zeroed()
            };
            let mut bits = ptr::null_mut();
            let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
            assert!(!bitmap.is_null(), "CreateDIBSection failed");
            let old = SelectObject(dc, bitmap);
            SetBkMode(dc, TRANSPARENT as i32);
            Self {
                w,
                h,
                dc,
                bitmap,
                old,
                bits: bits as *mut u32,
                fonts: Vec::new(),
                old_font: None,
                ink: None,
            }
        }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    fn px(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }

    fn clip(&mut self, r: Rect) -> Option<(usize, usize, usize, usize)> {
        let x0 = r.x.max(0);
        let y0 = r.y.max(0);
        let x1 = r.right().min(self.w);
        let y1 = r.bottom().min(self.h);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let drawn = Rect::new(x0, y0, x1 - x0, y1 - y0);
        self.ink = Some(self.ink.map_or(drawn, |i| i.union(&drawn)));
        Some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
    }

    /// The area drawn since the last call; tracking starts afresh.
    pub fn take_ink(&mut self) -> Option<Rect> {
        self.ink.take()
    }

    pub fn clear(&mut self, c: Color) {
        self.px().fill(c.0);
        self.ink = Some(self.bounds());
    }

    /// Overwrite `r` with `c` (no blending).
    pub fn fill(&mut self, r: Rect, c: Color) {
        let Some((x0, y0, x1, y1)) = self.clip(r) else {
            return;
        };
        let w = self.w as usize;
        let px = self.px();
        for y in y0..y1 {
            px[y * w + x0..y * w + x1].fill(c.0);
        }
    }

    /// Blend `c` over every pixel of `r` grown by `reach`, weighted by the
    /// coverage `cover` returns for the pixel centre.
    fn blend(&mut self, r: Rect, reach: i32, c: Color, cover: impl Fn(f32, f32) -> f32) {
        let Some((x0, y0, x1, y1)) = self.clip(r.inset(-reach)) else {
            return;
        };
        let w = self.w as usize;
        let px = self.px();
        for y in y0..y1 {
            let fy = y as f32 + 0.5;
            for x in x0..x1 {
                let k = cover(x as f32 + 0.5, fy);
                if k <= 0.0 {
                    continue;
                }
                let i = y * w + x;
                px[i] = over(c.scale(k), px[i]);
            }
        }
    }

    /// Anti-aliased rounded rectangle.
    pub fn round_rect(&mut self, r: Rect, radius: i32, c: Color) {
        let sdf = rounded(r, radius);
        self.blend(r, 1, c, |x, y| (0.5 - sdf(x, y)).clamp(0.0, 1.0));
    }

    /// Outline of a rounded rectangle, `t` wide, centred on its edge.
    pub fn round_stroke(&mut self, r: Rect, radius: i32, t: f32, c: Color) {
        let sdf = rounded(r, radius);
        let half = t / 2.0;
        self.blend(r, half.ceil() as i32 + 1, c, |x, y| {
            (half + 0.5 - sdf(x, y).abs()).clamp(0.0, 1.0)
        });
    }

    /// Soft drop shadow under a rounded rectangle.
    pub fn shadow(&mut self, r: Rect, radius: i32, blur: i32, dy: i32, alpha: u8) {
        let r = Rect::new(r.x, r.y + dy, r.w, r.h);
        let sdf = rounded(r, radius);
        let b = blur.max(1) as f32;
        self.blend(r, blur + 1, Color::rgba(0, 0, 0, alpha), |x, y| {
            let t = ((b - sdf(x, y)) / (2.0 * b)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        });
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Color) {
        let d = (2.0 * r).ceil() as i32 + 1;
        let bounds = Rect::new((cx - r) as i32, (cy - r) as i32, d, d);
        self.blend(bounds, 1, c, |x, y| {
            (r + 0.5 - (x - cx).hypot(y - cy)).clamp(0.0, 1.0)
        });
    }

    /// Part of a ring `t` wide: from `start` to `end` as fractions of a
    /// turn, clockwise from 12 o'clock.
    #[allow(clippy::too_many_arguments)]
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, t: f32, start: f32, end: f32, c: Color) {
        let reach = r + t;
        let d = (2.0 * reach) as i32 + 2;
        let bounds = Rect::new((cx - reach) as i32, (cy - reach) as i32, d, d);
        self.blend(bounds, 1, c, |x, y| {
            let (dx, dy) = (x - cx, y - cy);
            let radial = (t / 2.0 + 0.5 - (dx.hypot(dy) - r).abs()).clamp(0.0, 1.0);
            if radial <= 0.0 {
                return 0.0;
            }
            let turn = (dx.atan2(-dy) / std::f32::consts::TAU).rem_euclid(1.0);
            if (start..=end).contains(&turn) {
                radial
            } else {
                0.0
            }
        });
    }

    /// Anti-aliased line with round caps.
    pub fn line(&mut self, (x0, y0): (f32, f32), (x1, y1): (f32, f32), width: f32, c: Color) {
        let bounds = Rect::new(
            x0.min(x1) as i32,
            y0.min(y1) as i32,
            (x1 - x0).abs() as i32 + 1,
            (y1 - y0).abs() as i32 + 1,
        );
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        self.blend(bounds, width.ceil() as i32 + 1, c, |x, y| {
            let k = (((x - x0) * dx + (y - y0) * dy) / len2).clamp(0.0, 1.0);
            let d = (x - (x0 + k * dx)).hypot(y - (y0 + k * dy));
            (width / 2.0 + 0.5 - d).clamp(0.0, 1.0)
        });
    }

    fn select_font(&mut self, size: i32, weight: Weight) {
        let f = match self.fonts.iter().find(|f| f.0 == size && f.1 == weight) {
            Some(&(_, _, f)) => f,
            None => {
                let f = create_font(face(), size, weight);
                self.fonts.push((size, weight, f));
                f
            }
        };
        let prev = unsafe { SelectObject(self.dc, f) };
        self.old_font.get_or_insert(prev);
    }

    /// Width and height of `s` in pixels.
    pub fn measure(&mut self, s: &str, size: i32, weight: Weight) -> (i32, i32) {
        self.select_font(size, weight);
        let text: Vec<u16> = s.encode_utf16().collect();
        let mut sz = SIZE { cx: 0, cy: 0 };
        unsafe { GetTextExtentPoint32W(self.dc, text.as_ptr(), text.len() as i32, &mut sz) };
        (sz.cx, sz.cy)
    }

    /// Draw text with its top-left at (`x`, `y`), over an opaque background
    /// only: GDI zeroes the alpha of pixels it touches, so they're made
    /// opaque again afterwards (which is also what lets ClearType work).
    pub fn text(&mut self, x: i32, y: i32, s: &str, size: i32, weight: Weight, c: Color) {
        let (tw, th) = self.measure(s, size, weight);
        let text: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            SetTextColor(self.dc, c.colorref());
            TextOutW(self.dc, x, y, text.as_ptr(), text.len() as i32);
        }
        let Some((x0, y0, x1, y1)) = self.clip(Rect::new(x, y, tw, th)) else {
            return;
        };
        let w = self.w as usize;
        let px = self.px();
        for y in y0..y1 {
            for p in &mut px[y * w + x0..y * w + x1] {
                if *p >> 24 == 0 {
                    *p |= 0xFF00_0000;
                }
            }
        }
    }

    /// Text centred in `r`.
    pub fn text_in(&mut self, r: Rect, s: &str, size: i32, weight: Weight, c: Color) {
        let (tw, th) = self.measure(s, size, weight);
        self.text(
            r.x + (r.w - tw) / 2,
            r.y + (r.h - th) / 2,
            s,
            size,
            weight,
            c,
        );
    }

    /// Show the whole canvas as `hwnd`'s content, placed at screen (`x`, `y`).
    pub fn present(&self, hwnd: HWND, x: i32, y: i32) {
        self.present_dirty(hwnd, x, y, None, 255);
    }

    /// Like [`present`](Self::present), but only `dirty` is re-uploaded
    /// (the compositor keeps the rest), with an overall `opacity`.
    pub fn present_dirty(&self, hwnd: HWND, x: i32, y: i32, dirty: Option<Rect>, opacity: u8) {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: opacity,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let dirty = dirty.map(|d| RECT {
            left: d.x.max(0),
            top: d.y.max(0),
            right: d.right().min(self.w),
            bottom: d.bottom().min(self.h),
        });
        let dst = POINT { x, y };
        let size = SIZE {
            cx: self.w,
            cy: self.h,
        };
        let src = POINT { x: 0, y: 0 };
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: ptr::null_mut(),
            pptDst: &dst,
            psize: &size,
            hdcSrc: self.dc,
            pptSrc: &src,
            crKey: 0,
            pblend: &blend,
            dwFlags: ULW_ALPHA,
            prcDirty: dirty.as_ref().map_or(ptr::null(), |d| d as *const RECT),
        };
        unsafe { UpdateLayeredWindowIndirect(hwnd, &info) };
    }
}

/// Signed distance to a rounded rectangle (negative inside).
fn rounded(r: Rect, radius: i32) -> impl Fn(f32, f32) -> f32 {
    let rad = radius.min(r.w / 2).min(r.h / 2).max(0) as f32;
    let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
    let (bx, by) = (r.w as f32 / 2.0 - rad, r.h as f32 / 2.0 - rad);
    move |x, y| {
        let qx = (x - cx).abs() - bx;
        let qy = (y - cy).abs() - by;
        qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - rad
    }
}

/// Segoe UI Variable on Windows 11, Segoe UI before that.
fn face() -> &'static str {
    static FACE: OnceLock<&'static str> = OnceLock::new();
    FACE.get_or_init(|| {
        let want = "Segoe UI Variable Text";
        unsafe {
            let dc = CreateCompatibleDC(ptr::null_mut());
            let f = create_font(want, 12, Weight::Regular);
            let old = SelectObject(dc, f);
            let mut name = [0u16; 64];
            let n = GetTextFaceW(dc, name.len() as i32, name.as_mut_ptr());
            SelectObject(dc, old);
            DeleteObject(f);
            DeleteDC(dc);
            // The font mapper substitutes silently; check what we got.
            let len = name
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(n.max(0) as usize);
            if String::from_utf16_lossy(&name[..len]) == want {
                want
            } else {
                "Segoe UI"
            }
        }
    })
}

fn create_font(face: &str, size: i32, weight: Weight) -> HFONT {
    let face = wide(face);
    unsafe {
        CreateFontW(
            -size,
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            face.as_ptr(),
        )
    }
}

/// Premultiplied source-over.
fn over(src: u32, dst: u32) -> u32 {
    let ia = 255 - (src >> 24);
    let ch = |s: u32| {
        let v = ((src >> s) & 0xFF) + (((dst >> s) & 0xFF) * ia + 127) / 255;
        v.min(255) << s
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            if let Some(f) = self.old_font {
                SelectObject(self.dc, f);
            }
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            for &(_, _, f) in &self.fonts {
                DeleteObject(f);
            }
            DeleteDC(self.dc);
        }
    }
}
