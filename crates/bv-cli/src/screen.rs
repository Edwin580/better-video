//! The X screen's bounds on Linux, so a `--region` that runs off it is
//! refused with a sentence instead of ffmpeg's "Error opening input file
//! :0+600,400. Invalid argument".

use bv_core::CaptureRegion;

/// Bounds of the X screen `display` (or `$DISPLAY`), or `None` where that
/// can't be asked (not Linux, built without `input`, no X connection).
pub fn bounds(display: Option<&str>) -> Option<CaptureRegion> {
    backend::bounds(display)
}

/// Why `region` can't be recorded on a screen of `screen`'s size, if it
/// can't: it must lie wholly on the screen, as x11grab requires.
pub fn off_screen(region: CaptureRegion, screen: CaptureRegion) -> Option<String> {
    let right = i64::from(region.x) + i64::from(region.width);
    let bottom = i64::from(region.y) + i64::from(region.height);
    let fits = region.x >= screen.x
        && region.y >= screen.y
        && right <= i64::from(screen.x) + i64::from(screen.width)
        && bottom <= i64::from(screen.y) + i64::from(screen.height);
    (!fits).then(|| {
        format!(
            "--region {region} runs off the screen, which is {}x{}: it reaches \
             x = {right}, y = {bottom}. Make it smaller or move it.",
            screen.width, screen.height
        )
    })
}

#[cfg(all(target_os = "linux", feature = "input"))]
mod backend {
    use std::ffi::CString;

    use bv_core::CaptureRegion;
    use x11::xlib;

    pub fn bounds(display: Option<&str>) -> Option<CaptureRegion> {
        // XOpenDisplay(NULL) reads $DISPLAY itself.
        let name = display.map(CString::new).transpose().ok()?;
        // SAFETY: plain Xlib calls on a connection opened and closed here;
        // XOpenDisplay returns null on failure, which is checked.
        unsafe {
            let dpy = xlib::XOpenDisplay(name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()));
            if dpy.is_null() {
                return None;
            }
            let screen = xlib::XDefaultScreen(dpy);
            let (w, h) = (
                xlib::XDisplayWidth(dpy, screen),
                xlib::XDisplayHeight(dpy, screen),
            );
            xlib::XCloseDisplay(dpy);
            Some(CaptureRegion {
                x: 0,
                y: 0,
                width: u32::try_from(w).ok()?,
                height: u32::try_from(h).ok()?,
            })
        }
    }
}

#[cfg(not(all(target_os = "linux", feature = "input")))]
mod backend {
    use bv_core::CaptureRegion;

    pub fn bounds(_display: Option<&str>) -> Option<CaptureRegion> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(width: u32, height: u32, x: i32, y: i32) -> CaptureRegion {
        CaptureRegion {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn regions_on_the_screen_fit() {
        let screen = r(1280, 800, 0, 0);
        assert_eq!(off_screen(r(1280, 800, 0, 0), screen), None);
        assert_eq!(off_screen(r(1024, 640, 256, 160), screen), None);
        assert_eq!(off_screen(r(1, 1, 1279, 799), screen), None);
    }

    #[test]
    fn regions_past_an_edge_are_refused_with_the_screen_size() {
        let screen = r(1280, 800, 0, 0);
        let why = off_screen(r(1024, 640, 600, 400), screen).unwrap();
        assert!(why.contains("1024x640+600+400"), "{why}");
        assert!(why.contains("1280x800"), "{why}");
        assert!(why.contains("x = 1624, y = 1040"), "{why}");
        assert!(off_screen(r(10, 10, -1, 0), screen).is_some());
        assert!(off_screen(r(10, 10, 0, -1), screen).is_some());
        assert!(off_screen(r(1281, 1, 0, 0), screen).is_some());
        // Sizes near u32::MAX don't overflow.
        assert!(off_screen(r(u32::MAX, u32::MAX, i32::MAX, i32::MAX), screen).is_some());
    }
}
