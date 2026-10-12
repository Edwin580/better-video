//! Win32 plumbing shared by the overlay windows.
//!
//! Each UI runs on its own thread with its own message loop, so it never
//! re-enters the tray's event loop. Windows are layered popups drawn with
//! [`canvas::Canvas`]; a window's behaviour lives in a [`Handler`].

pub mod canvas;
pub mod indicator;
pub mod picker;

use std::{cell::Cell, ptr};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        GetWindowLongPtrW, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW,
        SetWindowLongPtrW, TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, MSG, WM_CLOSE,
        WM_DESTROY, WM_NCCREATE, WM_NCDESTROY, WNDCLASSEXW,
    },
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Window behaviour. Return `Some` to handle a message, `None` for the
/// default. To close, call [`close`] (never `DestroyWindow` from here).
pub trait Handler {
    fn message(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT>;
}

struct Slot {
    handler: Box<dyn Handler>,
    /// Set while the handler runs; nested messages (sent synchronously by
    /// calls the handler makes) go to the default procedure instead of
    /// aliasing the handler.
    busy: Cell<bool>,
    quit_on_destroy: bool,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
    }
    let slot = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Slot;
    if slot.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    match msg {
        WM_CLOSE => {
            DestroyWindow(hwnd);
            return 0;
        }
        WM_DESTROY => {
            if (*slot).quit_on_destroy {
                PostQuitMessage(0);
            }
            return 0;
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(slot));
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        _ => {}
    }
    if (*slot).busy.get() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    (*slot).busy.set(true);
    let handler: &mut dyn Handler = &mut *(*slot).handler;
    let result = handler.message(hwnd, msg, wparam, lparam);
    (*slot).busy.set(false);
    result.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam))
}

/// Create a top-level window of class `class` (registered on first use
/// with `cursor`, e.g. `IDC_CROSS`). When `quit_on_destroy`, closing it
/// ends the thread's [`run_loop`].
pub fn create(
    class: &str,
    cursor: *const u16,
    ex_style: u32,
    style: u32,
    (x, y, w, h): (i32, i32, i32, i32),
    handler: Box<dyn Handler>,
    quit_on_destroy: bool,
) -> HWND {
    let name = wide(class);
    unsafe {
        let instance = GetModuleHandleW(ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(ptr::null_mut(), cursor),
            lpszClassName: name.as_ptr(),
            ..std::mem::zeroed()
        };
        // Fails harmlessly if the class already exists.
        RegisterClassExW(&wc);
        let slot = Box::into_raw(Box::new(Slot {
            handler,
            busy: Cell::new(false),
            quit_on_destroy,
        }));
        let title = wide("better-video");
        CreateWindowExW(
            ex_style,
            name.as_ptr(),
            title.as_ptr(),
            style,
            x,
            y,
            w,
            h,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            slot as *const _,
        )
    }
}

/// Ask a window to close (safe from inside its handler).
pub fn close(hwnd: HWND) {
    unsafe {
        PostMessageW(hwnd, WM_CLOSE, 0, 0);
    }
}

/// Pump messages until the thread's quit message.
pub fn run_loop() {
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Signed client coordinates from a mouse message's `lparam`.
pub fn mouse_pos(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam & 0xFFFF) as i16 as i32,
        ((lparam >> 16) & 0xFFFF) as i16 as i32,
    )
}
