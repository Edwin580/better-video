//! A small notification in the bottom-right corner of the main screen.
//! Clicking it opens Explorer on the file; it fades on its own after a few
//! seconds. Hidden from screen capture so it never lands in a recording.

use std::{path::PathBuf, process::Command, thread};

use bv_core::monitor;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    UI::WindowsAndMessaging::{
        SetTimer, SetWindowDisplayAffinity, ShowWindow, SystemParametersInfoW, IDC_HAND,
        SPI_GETWORKAREA, SW_SHOWNOACTIVATE, WM_LBUTTONUP, WM_TIMER, WS_EX_LAYERED,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    },
};

use super::{
    canvas::{Canvas, Color, Rect},
    close, create, run_loop, Handler,
};

const BG: Color = Color::rgb(32, 32, 36);
const SHOW_MS: u32 = 7000;
const EXCLUDE_FROM_CAPTURE: u32 = 0x11;

/// Show a notification without blocking.
pub fn show(title: String, body: String, reveal: Option<PathBuf>) {
    let _ = thread::Builder::new()
        .name("bv-toast".into())
        .spawn(move || open(title, body, reveal));
}

fn open(title: String, body: String, reveal: Option<PathBuf>) {
    let mut work = RECT {
        left: 0,
        top: 0,
        right: 1280,
        bottom: 720,
    };
    unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as *mut _, 0) };
    let scale = monitor::monitors()
        .iter()
        .find(|m| m.primary)
        .map_or(100, |m| m.scale as i32);
    let px = |v: i32| v * scale / 100;
    let (w, h) = (px(400), px(76));
    let (x, y) = (work.right - w - px(16), work.bottom - h - px(16));

    let mut canvas = Canvas::new(w, h);
    canvas.clear(Color(0));
    canvas.round_rect(Rect::new(0, 0, w, h), px(10), BG);
    let pad = px(16);
    let (title_size, body_size) = (px(15), px(12));
    let title = fit(&mut canvas, &title, title_size, true, w - 2 * pad);
    let body = fit(&mut canvas, &body, body_size, false, w - 2 * pad);
    canvas.text(pad, px(14), &title, title_size, true, [255, 255, 255]);
    canvas.text(pad, px(42), &body, body_size, false, [190, 190, 200]);

    let hwnd = create(
        "bv-toast",
        IDC_HAND,
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        WS_POPUP,
        (x, y, w, h),
        Box::new(Toast { reveal }),
        true,
    );
    if hwnd.is_null() {
        return;
    }
    unsafe {
        SetWindowDisplayAffinity(hwnd, EXCLUDE_FROM_CAPTURE);
        canvas.present(hwnd, x, y);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, 1, SHOW_MS, None);
    }
    run_loop();
}

/// Shorten `s` with an ellipsis in the middle until it fits `max` pixels
/// (paths keep their start and their file name).
fn fit(canvas: &mut Canvas, s: &str, size: i32, bold: bool, max: i32) -> String {
    if canvas.measure(s, size, bold).0 <= max {
        return s.to_owned();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut keep = chars.len();
    while keep > 4 {
        keep -= 1;
        let head = keep / 2;
        let tail = keep - head;
        let t: String = chars[..head]
            .iter()
            .chain(['…'].iter())
            .chain(&chars[chars.len() - tail..])
            .collect();
        if canvas.measure(&t, size, bold).0 <= max {
            return t;
        }
    }
    s.chars().take(4).collect()
}

struct Toast {
    reveal: Option<PathBuf>,
}

impl Handler for Toast {
    fn message(&mut self, hwnd: HWND, msg: u32, _: WPARAM, _: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_TIMER => {
                close(hwnd);
                Some(0)
            }
            WM_LBUTTONUP => {
                if let Some(p) = &self.reveal {
                    reveal(p);
                }
                close(hwnd);
                Some(0)
            }
            _ => None,
        }
    }
}

/// Open Explorer with `path` selected (or the folder itself).
pub fn reveal(path: &std::path::Path) {
    let result = if path.is_dir() {
        Command::new("explorer").arg(path).spawn()
    } else {
        // explorer wants `/select,` and the path as one argument.
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        Command::new("explorer").arg(arg).spawn()
    };
    if let Err(e) = result {
        eprintln!("error: couldn't open Explorer: {e}");
    }
}
