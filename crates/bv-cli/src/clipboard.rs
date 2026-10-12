//! Put an exported file on the system clipboard so it can be pasted
//! straight into a PR comment, chat, or file manager.
//!
//! On Windows this is done in-process (`CF_HDROP`, what Explorer's Copy
//! puts there), so it works where PowerShell is locked down. macOS and
//! Linux use the platform's clipboard tool.

use std::{
    env,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};
use bv_core::ffmpeg::Platform;

/// A clipboard tool invocation: program, args, and data for its stdin.
#[derive(Debug, PartialEq)]
struct Invocation {
    program: &'static str,
    args: Vec<String>,
    stdin: Option<String>,
}

/// The external tool to run, if the platform needs one.
fn invocation(platform: Platform, path: &Path, wayland: bool) -> Option<Invocation> {
    let p = path.to_string_lossy();
    match platform {
        // Copies the file itself (like ⌘C in Finder).
        Platform::MacOs => Some(Invocation {
            program: "osascript",
            args: vec![
                "-e".into(),
                format!(
                    "set the clipboard to (POSIX file \"{}\")",
                    applescript_escape(&p)
                ),
            ],
            stdin: None,
        }),
        // Native, see `windows::copy_file`.
        Platform::Windows => None,
        // File managers and most apps accept a file URI list.
        Platform::Linux => {
            let uri = file_uri(path);
            Some(if wayland {
                Invocation {
                    program: "wl-copy",
                    args: vec!["--type".into(), "text/uri-list".into()],
                    stdin: Some(uri),
                }
            } else {
                Invocation {
                    program: "xclip",
                    args: ["-selection", "clipboard", "-t", "text/uri-list"]
                        .map(String::from)
                        .to_vec(),
                    stdin: Some(uri),
                }
            })
        }
    }
}

pub fn copy_file(path: &Path) -> Result<()> {
    let path = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;
    let wayland = env::var_os("WAYLAND_DISPLAY").is_some();
    let Some(inv) = invocation(Platform::current(), &path, wayland) else {
        #[cfg(windows)]
        return windows::copy_file(&path);
        #[cfg(not(windows))]
        unreachable!("only Windows copies natively");
    };

    let mut child = Command::new(inv.program)
        .args(&inv.args)
        .stdin(if inv.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .spawn()
        .with_context(|| format!("running {} (is it installed?)", inv.program))?;
    if let Some(data) = &inv.stdin {
        let mut stdin = child.stdin.take().context("clipboard stdin")?;
        stdin.write_all(data.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("{} failed ({status})", inv.program);
    }
    Ok(())
}

#[cfg(windows)]
mod windows {
    use std::{mem, os::windows::ffi::OsStrExt, path::Path, ptr, thread, time::Duration};

    use anyhow::{bail, Result};
    use windows_sys::Win32::{
        Foundation::{GlobalFree, POINT},
        System::{
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
        },
        UI::Shell::DROPFILES,
    };

    /// Standard clipboard format for a list of files.
    const CF_HDROP: u32 = 15;

    /// Put `path` on the clipboard as a copied file.
    pub fn copy_file(path: &Path) -> Result<()> {
        let text = path.as_os_str().to_string_lossy();
        let path = Path::new(super::without_verbatim_prefix(&text));
        // DROPFILES header, then the path as UTF-16, then two NULs (the
        // list of paths is double-NUL terminated).
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
        let header = mem::size_of::<DROPFILES>();
        let size = header + wide.len() * 2;
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, size);
            if handle.is_null() {
                bail!("out of memory for the clipboard");
            }
            let data = GlobalLock(handle) as *mut u8;
            if data.is_null() {
                GlobalFree(handle);
                bail!("couldn't lock clipboard memory");
            }
            let drop = DROPFILES {
                pFiles: header as u32,
                pt: POINT { x: 0, y: 0 },
                fNC: 0,
                fWide: 1,
            };
            ptr::write_unaligned(data as *mut DROPFILES, drop);
            ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, data.add(header), wide.len() * 2);
            GlobalUnlock(handle);

            // Another app may hold the clipboard for a moment.
            let mut opened = false;
            for _ in 0..20 {
                if OpenClipboard(ptr::null_mut()) != 0 {
                    opened = true;
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            if !opened {
                GlobalFree(handle);
                bail!("the clipboard is busy (another app has it open)");
            }
            EmptyClipboard();
            // On success the clipboard owns the memory.
            let set = !SetClipboardData(CF_HDROP, handle).is_null();
            CloseClipboard();
            if !set {
                GlobalFree(handle);
                bail!("couldn't set the clipboard");
            }
        }
        Ok(())
    }
}
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `file://` URI with RFC 3986 percent-encoding.
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(b as char)
            }
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri.push_str("\r\n");
    uri
}

/// `canonicalize()` on Windows adds the `\\?\` prefix, which some paste
/// targets don't understand; drop it for ordinary drive paths (UNC paths
/// keep theirs, where it's meaningful).
#[cfg_attr(not(windows), allow(dead_code))]
fn without_verbatim_prefix(path: &str) -> &str {
    match path.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with(r"UNC\") => rest,
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_only_the_drive_verbatim_prefix() {
        assert_eq!(
            without_verbatim_prefix(r"\\?\C:\clips\a.gif"),
            r"C:\clips\a.gif"
        );
        assert_eq!(
            without_verbatim_prefix(r"C:\clips\a.gif"),
            r"C:\clips\a.gif"
        );
        let unc = r"\\?\UNC\server\share\a.gif";
        assert_eq!(without_verbatim_prefix(unc), unc);
    }

    #[test]
    fn linux_uses_uri_list() {
        let inv = invocation(Platform::Linux, Path::new("/tmp/my clip.gif"), false).unwrap();
        assert_eq!(inv.program, "xclip");
        assert_eq!(inv.stdin.as_deref(), Some("file:///tmp/my%20clip.gif\r\n"));
        let inv = invocation(Platform::Linux, Path::new("/tmp/a.gif"), true).unwrap();
        assert_eq!(inv.program, "wl-copy");
    }

    #[test]
    fn quotes_are_escaped() {
        let inv = invocation(Platform::MacOs, Path::new("/tmp/a\"b.gif"), false).unwrap();
        assert_eq!(
            inv.args[1],
            r#"set the clipboard to (POSIX file "/tmp/a\"b.gif")"#
        );
    }

    #[test]
    fn windows_needs_no_external_tool() {
        assert!(invocation(Platform::Windows, Path::new(r"C:\a.gif"), false).is_none());
    }
}
