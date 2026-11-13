//! Where recordings are saved and how they're named, so every mode
//! (`record`, `hotkey`, `tray`) puts clips in one predictable place and
//! `bv list` / `bv open` can find them again.
//!
//! ```text
//! ~/Videos/better-video/            (~/Movies on macOS; BV_DIR overrides)
//!   bv-2026-11-13_15-48-12.mp4      local time, so names sort by when
//!   bv-2026-11-13_15-48-12-2.mp4    a second take in the same second
//! ```

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use bv_core::OutputFormat;

/// Environment variable naming the recordings folder.
pub const DIR_VAR: &str = "BV_DIR";

/// The recordings folder: `$BV_DIR`, else `~/Movies/better-video` on macOS
/// and `~/Videos/better-video` elsewhere.
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os(DIR_VAR).filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let videos = if cfg!(target_os = "macos") {
        "Movies"
    } else {
        "Videos"
    };
    home.join(videos).join("better-video")
}

/// A new file in `dir` named after the current time, with `-2`, `-3`, …
/// added if a recording from the same second already exists.
pub fn fresh_output(dir: &Path, format: OutputFormat) -> PathBuf {
    let first = dir.join(default_name(format));
    let stem = first.with_extension("");
    let numbered = |n: u32| {
        let mut name = stem.clone().into_os_string();
        name.push(format!("-{n}.{}", format.extension()));
        PathBuf::from(name)
    };
    std::iter::once(first.clone())
        .chain((2..).map(numbered))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// `bv-2026-10-11_15-48-12.gif`, in local time.
pub fn default_name(format: OutputFormat) -> PathBuf {
    let (y, mo, d, h, mi, s) = now();
    PathBuf::from(format!(
        "bv-{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}.{}",
        format.extension()
    ))
}

/// A finished clip in the recordings folder.
pub struct Clip {
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: SystemTime,
}

impl Clip {
    pub fn json(&self) -> serde_json::Value {
        let unix = self
            .modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let format = OutputFormat::from_path(&self.path).map_or("unknown", |f| f.extension());
        serde_json::json!({
            "path": crate::absolute(&self.path),
            "bytes": self.bytes,
            "format": format,
            "modified": unix,
        })
    }
}

/// The GIF/MP4/WebM files in `dir` (not its subfolders), newest first.
/// Raw takes and cursor tracks are left out. Empty if `dir` doesn't exist.
pub fn clips(dir: &Path) -> Vec<Clip> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut clips: Vec<Clip> = entries
        .flatten()
        .filter(|e| OutputFormat::from_path(&e.path()).is_some())
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(|m| m.is_file())?;
            Some(Clip {
                path: e.path(),
                bytes: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            })
        })
        .collect();
    // Newest first; the time-stamped names break ties within a second.
    clips.sort_by(|a, b| b.modified.cmp(&a.modified).then(b.path.cmp(&a.path)));
    clips
}

/// "just now", "5 min ago", "3 h ago", "2 d ago".
pub fn age(t: SystemTime) -> String {
    let secs = t.elapsed().map_or(0, |d| d.as_secs());
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// Open a file or folder with its default app.
pub fn open_path(path: &Path) -> std::io::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    Command::new(program).arg(path).spawn().map(drop)
}

#[cfg(windows)]
fn now() -> (u32, u32, u32, u32, u32, u32) {
    let mut t = unsafe { std::mem::zeroed() };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t) };
    let t: windows_sys::Win32::Foundation::SYSTEMTIME = t;
    let f = |v: u16| v as u32;
    (
        f(t.wYear),
        f(t.wMonth),
        f(t.wDay),
        f(t.wHour),
        f(t.wMinute),
        f(t.wSecond),
    )
}

#[cfg(not(windows))]
fn now() -> (u32, u32, u32, u32, u32, u32) {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    local_time(secs).unwrap_or_else(|| utc_time(secs))
}

/// `secs` since the epoch in the local time zone, so a clip recorded at
/// 3 pm is named 15-…, not whatever the time is in Greenwich.
#[cfg(unix)]
fn local_time(secs: u64) -> Option<(u32, u32, u32, u32, u32, u32)> {
    let t = libc::time_t::try_from(secs).ok()?;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    let u = |v: libc::c_int| u32::try_from(v).ok();
    Some((
        u(tm.tm_year + 1900)?,
        u(tm.tm_mon + 1)?,
        u(tm.tm_mday)?,
        u(tm.tm_hour)?,
        u(tm.tm_min)?,
        u(tm.tm_sec)?,
    ))
}

#[cfg(not(any(unix, windows)))]
fn local_time(_secs: u64) -> Option<(u32, u32, u32, u32, u32, u32)> {
    None
}

#[cfg(not(windows))]
fn utc_time(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    (
        y as u32,
        m,
        d,
        (rem / 3600) as u32,
        (rem / 60 % 60) as u32,
        (rem % 60) as u32,
    )
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
#[cfg(not(windows))]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("better-video-test")
            .join(format!("library-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fresh_output_never_overwrites() {
        let dir = test_dir("fresh");
        let a = fresh_output(&dir, OutputFormat::Gif);
        std::fs::write(&a, b"x").unwrap();
        let b = fresh_output(&dir, OutputFormat::Gif);
        assert_ne!(a, b);
        assert!(!b.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn names_sort_by_time() {
        let name = default_name(OutputFormat::Gif);
        let name = name.to_string_lossy();
        assert!(
            name.starts_with("bv-20") && name.ends_with(".gif"),
            "{name}"
        );
        assert_eq!(name.len(), "bv-2026-10-11_15-48-12.gif".len());
    }

    #[test]
    fn lists_only_clips_newest_first() {
        let dir = test_dir("list");
        for name in [
            "old.gif",
            "new.mp4",
            "take.raw.mkv",
            "take.raw.cursor.json",
            "notes.txt",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        std::fs::create_dir(dir.join("folder.gif")).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3_600);
        std::fs::File::options()
            .write(true)
            .open(dir.join("old.gif"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let names: Vec<_> = clips(&dir)
            .iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["new.mp4", "old.gif"]);
        assert!(clips(&dir.join("missing")).is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn ages_read_naturally() {
        let ago = |s| age(SystemTime::now() - Duration::from_secs(s));
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(300), "5 min ago");
        assert_eq!(ago(3 * 3_600 + 5), "3 h ago");
        assert_eq!(ago(2 * 86_400), "2 d ago");
    }

    #[cfg(not(windows))]
    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_737), (2026, 10, 11));
    }

    #[cfg(unix)]
    #[test]
    fn local_time_is_a_real_date() {
        let (y, mo, d, h, mi, s) = local_time(1_760_197_692).unwrap();
        assert!((2025..=2026).contains(&y));
        assert!((1..=12).contains(&mo) && (1..=31).contains(&d));
        assert!(h < 24 && mi < 60 && s < 61);
    }
}
