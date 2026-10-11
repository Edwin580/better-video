//! Tray preferences remembered between runs, in
//! `%APPDATA%\better-video\settings.json` (`~/.config/better-video` elsewhere).

use std::{env, fs, path::PathBuf};

use bv_core::CaptureRegion;
use serde::{Deserialize, Serialize};

use crate::ui::Choices;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Folder recordings are saved in.
    pub dir: Option<PathBuf>,
    /// Last options-bar choices.
    pub choices: Option<Choices>,
    /// Copy each recording to the clipboard.
    pub copy: Option<bool>,
    /// Last recorded area, for "Record same area again".
    pub last_area: Option<CaptureRegion>,
}

fn path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        env::var_os("APPDATA").map(PathBuf::from)
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    Some(base?.join("better-video").join("settings.json"))
}

impl Settings {
    /// Saved settings, or defaults if there are none (or they're unreadable).
    pub fn load() -> Self {
        path()
            .and_then(|p| fs::read(p).ok())
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        let result = p
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&p, serde_json::to_vec_pretty(self).unwrap_or_default()));
        if let Err(e) = result {
            eprintln!("warning: couldn't save settings to {}: {e}", p.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Quality;
    use bv_core::OutputFormat;

    #[test]
    fn round_trips_and_tolerates_missing_fields() {
        let s = Settings {
            dir: Some("C:\\clips".into()),
            choices: Some(Choices {
                format: OutputFormat::Gif,
                quality: Quality::Small,
                clicks: true,
                zoom: false,
            }),
            copy: Some(false),
            last_area: Some("640x480-100+0".parse().unwrap()),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Settings>(&json).unwrap(), s);
        let partial: Settings = serde_json::from_str(r#"{"copy": true}"#).unwrap();
        assert_eq!(partial.copy, Some(true));
        assert_eq!(partial.dir, None);
    }
}
