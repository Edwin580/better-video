//! Put an exported file on the system clipboard so it can be pasted
//! straight into a PR comment, chat, or file manager.

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

fn invocation(platform: Platform, path: &Path, wayland: bool) -> Invocation {
    let p = path.to_string_lossy();
    match platform {
        // Copies the file itself (like ⌘C in Finder).
        Platform::MacOs => Invocation {
            program: "osascript",
            args: vec![
                "-e".into(),
                format!(
                    "set the clipboard to (POSIX file \"{}\")",
                    applescript_escape(&p)
                ),
            ],
            stdin: None,
        },
        // Copies the file itself (like Ctrl+C in Explorer).
        Platform::Windows => Invocation {
            program: "powershell",
            args: vec![
                "-NoProfile".into(),
                "-Command".into(),
                format!("Set-Clipboard -LiteralPath '{}'", p.replace('\'', "''")),
            ],
            stdin: None,
        },
        // File managers and most apps accept a file URI list.
        Platform::Linux => {
            let uri = file_uri(path);
            if wayland {
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
            }
        }
    }
}

pub fn copy_file(path: &Path) -> Result<()> {
    let path = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;
    let wayland = env::var_os("WAYLAND_DISPLAY").is_some();
    let inv = invocation(Platform::current(), &path, wayland);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_uses_uri_list() {
        let inv = invocation(Platform::Linux, Path::new("/tmp/my clip.gif"), false);
        assert_eq!(inv.program, "xclip");
        assert_eq!(inv.stdin.as_deref(), Some("file:///tmp/my%20clip.gif\r\n"));
        let inv = invocation(Platform::Linux, Path::new("/tmp/a.gif"), true);
        assert_eq!(inv.program, "wl-copy");
    }

    #[test]
    fn quotes_are_escaped() {
        let inv = invocation(Platform::MacOs, Path::new("/tmp/a\"b.gif"), false);
        assert_eq!(
            inv.args[1],
            r#"set the clipboard to (POSIX file "/tmp/a\"b.gif")"#
        );
        let inv = invocation(Platform::Windows, Path::new("C:\\it's.gif"), false);
        assert_eq!(inv.args[2], "Set-Clipboard -LiteralPath 'C:\\it''s.gif'");
    }
}
