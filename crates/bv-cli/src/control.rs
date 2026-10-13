//! Lets `bv stop` end recordings started in the background (by a script
//! or an AI agent): each `bv record` registers itself in
//! `<temp>/better-video/active` and watches for a stop request there.
//!
//! ```text
//! <pid>.json         a recording is running (its output path)
//! <pid>.stop         `bv stop` asks it to finish
//! <pid>.result.json  what it produced, for `bv stop` to print
//! ```

use std::{
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde_json::{json, Value};

fn dir() -> PathBuf {
    std::env::temp_dir().join("better-video").join("active")
}

/// A running recording, registered for `bv stop`. Unregisters on drop.
pub struct Active {
    dir: PathBuf,
    pid: u32,
}

impl Active {
    pub fn register(output: &Path) -> Result<Self> {
        Self::register_in(dir(), output)
    }

    fn register_in(dir: PathBuf, output: &Path) -> Result<Self> {
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let pid = std::process::id();
        let me = Self { dir, pid };
        let _ = fs::remove_file(me.file("stop"));
        let info = json!({ "pid": pid, "output": output });
        fs::write(me.file("json"), info.to_string())?;
        Ok(me)
    }

    fn file(&self, ext: &str) -> PathBuf {
        self.dir.join(format!("{}.{ext}", self.pid))
    }

    pub fn stop_requested(&self) -> bool {
        self.file("stop").exists()
    }

    /// Leave the result for `bv stop` and unregister.
    pub fn done(self, result: &Value) {
        let _ = fs::write(self.file("result.json"), result.to_string());
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.file("json"));
        let _ = fs::remove_file(self.file("stop"));
    }
}

/// Ask every running recording to stop, wait for them to export, and
/// return their results.
pub fn stop_all(timeout: Duration) -> Result<Vec<Value>> {
    stop_all_in(&dir(), timeout)
}

fn stop_all_in(dir: &Path, timeout: Duration) -> Result<Vec<Value>> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name
            .strip_suffix(".json")
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if alive(pid) {
            fs::write(dir.join(format!("{pid}.stop")), b"")?;
            pids.push(pid);
        } else {
            // Left behind by a recording that was killed.
            let _ = fs::remove_file(entry.path());
        }
    }

    let deadline = Instant::now() + timeout;
    let mut results = Vec::new();
    for pid in pids {
        let running = dir.join(format!("{pid}.json"));
        while running.exists() && alive(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
        let result = dir.join(format!("{pid}.result.json"));
        let value = fs::read_to_string(&result)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| {
                let state = if running.exists() {
                    "still exporting"
                } else {
                    "ended without a result"
                };
                json!({ "pid": pid, "error": state })
            });
        let _ = fs::remove_file(result);
        results.push(value);
    }
    Ok(results)
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // Signal 0 checks the process exists without touching it; EPERM means
    // it exists but belongs to someone else.
    let found = unsafe { libc::kill(pid, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(any(windows, unix)))]
fn alive(_pid: u32) -> bool {
    // Unknown platform: assume it's alive (the wait has a timeout).
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own per test, so parallel tests (and real
    /// recordings) don't see each other's entries.
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("better-video-test")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn stale_registrations_are_cleared() {
        // A pid that can't be running.
        let dir = test_dir("stale");
        let stale = dir.join("4294967290.json");
        fs::write(&stale, "{}").unwrap();
        let results = stop_all_in(&dir, Duration::from_millis(10)).unwrap();
        assert!(!stale.exists());
        assert!(results.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn this_process_is_alive() {
        assert!(alive(std::process::id()));
    }

    #[test]
    fn registration_round_trip() {
        let dir = test_dir("round-trip");
        let me = Active::register_in(dir.clone(), Path::new("clip.gif")).unwrap();
        assert!(me.file("json").exists());
        assert!(!me.stop_requested());
        fs::write(me.file("stop"), b"").unwrap();
        assert!(me.stop_requested());
        let json = me.file("json");
        let result = me.file("result.json");
        me.done(&json!({ "path": "clip.gif" }));
        assert!(!json.exists());
        assert!(result.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
