//! A premultiplied-BGRA drawing surface for layered windows: a DIB section
//! we draw into directly, presented with `UpdateLayeredWindow`.

use std::{mem, ptr};

use windows_sys::Win32::{
    Foundation::{HWND, POINT, SIZE},
    Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, GetDC,
        GetTextExtentPoint32W, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TextOutW,
        AC_SRC_ALPHA, AC_SRC_OVER, ANTIALIASED_QUALITY, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
        BLENDFUNCTION, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS,
        HBITMAP, HDC, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS, TRANSPARENT,
    },
    UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA},
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

    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
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

    fn scale(self, k: u32) -> u32 {
        let c = self.0;
        let ch = |s: u32| (((c >> s) & 0xFF) * k / 255) << s;
        ch(24) | ch(16) | ch(8) | ch(0)
    }
}

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    fonts: Vec<(i32, bool, HFONT)>,
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
            }
        }
    }

    fn px(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }

    pub fn clear(&mut self, c: Color) {
        self.px().fill(c.0);
    }

    fn clip(&self, r: Rect) -> Option<(usize, usize, usize, usize)> {
        let x0 = r.x.max(0);
        let y0 = r.y.max(0);
        let x1 = (r.x + r.w).min(self.w);
        let y1 = (r.y + r.h).min(self.h);
        (x1 > x0 && y1 > y0).then_some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
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

    /// A border of thickness `t` just outside `r`.
    pub fn outline(&mut self, r: Rect, t: i32, c: Color) {
        self.fill(Rect::new(r.x - t, r.y - t, r.w + 2 * t, t), c);
        self.fill(Rect::new(r.x - t, r.y + r.h, r.w + 2 * t, t), c);
        self.fill(Rect::new(r.x - t, r.y, t, r.h), c);
        self.fill(Rect::new(r.x + r.w, r.y, t, r.h), c);
    }

    /// Blend a rounded rectangle over what's there, anti-aliased corners.
    pub fn round_rect(&mut self, r: Rect, radius: i32, c: Color) {
        let Some((x0, y0, x1, y1)) = self.clip(r) else {
            return;
        };
        let w = self.w as usize;
        let rad = radius.min(r.w / 2).min(r.h / 2) as f32;
        let (l, t) = (r.x as f32, r.y as f32);
        let (rr, b) = ((r.x + r.w) as f32, (r.y + r.h) as f32);
        let px = self.px();
        for y in y0..y1 {
            let fy = y as f32 + 0.5;
            for x in x0..x1 {
                let fx = x as f32 + 0.5;
                // Distance into the nearest corner's quarter circle.
                let cx = fx.clamp(l + rad, rr - rad);
                let cy = fy.clamp(t + rad, b - rad);
                let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                let cover = (rad + 0.5 - d).clamp(0.0, 1.0);
                if cover <= 0.0 {
                    continue;
                }
                let src = c.scale((cover * 255.0) as u32);
                let i = y * w + x;
                px[i] = over(src, px[i]);
            }
        }
    }

    fn font(&mut self, size: i32, bold: bool) -> HFONT {
        if let Some(&(_, _, f)) = self.fonts.iter().find(|f| f.0 == size && f.1 == bold) {
            return f;
        }
        let face = wide("Segoe UI");
        let f = unsafe {
            CreateFontW(
                -size,
                0,
                0,
                0,
                if bold { 600 } else { 400 },
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                ANTIALIASED_QUALITY as u32,
                DEFAULT_PITCH as u32,
                face.as_ptr(),
            )
        };
        self.fonts.push((size, bold, f));
        f
    }

    /// Width and height of `s` in pixels.
    pub fn measure(&mut self, s: &str, size: i32, bold: bool) -> (i32, i32) {
        let f = self.font(size, bold);
        let text: Vec<u16> = s.encode_utf16().collect();
        let mut sz = SIZE { cx: 0, cy: 0 };
        unsafe {
            SelectObject(self.dc, f);
            GetTextExtentPoint32W(self.dc, text.as_ptr(), text.len() as i32, &mut sz);
        }
        (sz.cx, sz.cy)
    }

    /// Draw text with its top-left at (`x`, `y`). Draw it over an opaque
    /// background: GDI leaves alpha at zero, so afterwards every touched
    /// pixel in the text's box is made opaque again.
    pub fn text(&mut self, x: i32, y: i32, s: &str, size: i32, bold: bool, rgb: [u8; 3]) {
        let (tw, th) = self.measure(s, size, bold);
        let text: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            SetTextColor(
                self.dc,
                rgb[0] as u32 | (rgb[1] as u32) << 8 | (rgb[2] as u32) << 16,
            );
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

    /// A rounded label: dark pill with white text, centred on (`cx`, `cy`).
    /// Returns its rectangle.
    pub fn pill(&mut self, cx: i32, cy: i32, s: &str, size: i32, bg: Color) -> Rect {
        let (tw, th) = self.measure(s, size, true);
        let (padx, pady) = (size, size / 2);
        let r = Rect::new(
            cx - tw / 2 - padx,
            cy - th / 2 - pady,
            tw + 2 * padx,
            th + 2 * pady,
        );
        self.round_rect(r, r.h / 2, bg);
        self.text(r.x + padx, r.y + pady, s, size, true, [255, 255, 255]);
        r
    }

    /// Show the canvas as `hwnd`'s content, placed at screen (`x`, `y`).
    pub fn present(&self, hwnd: HWND, x: i32, y: i32) {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                ptr::null_mut(),
                &POINT { x, y },
                &SIZE {
                    cx: self.w,
                    cy: self.h,
                },
                self.dc,
                &POINT { x: 0, y: 0 },
                0,
                &blend,
                ULW_ALPHA,
            );
        }
    }
}

/// Premultiplied source-over.
fn over(src: u32, dst: u32) -> u32 {
    let ia = 255 - (src >> 24);
    let ch = |s: u32| {
        let v = ((src >> s) & 0xFF) + ((dst >> s) & 0xFF) * ia / 255;
        v.min(255) << s
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            for &(_, _, f) in &self.fonts {
                DeleteObject(f);
            }
            DeleteDC(self.dc);
        }
    }
}
