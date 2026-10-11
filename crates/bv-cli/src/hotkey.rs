//! Global hotkey, detected by polling the keyboard state.
//!
//! Polling (rather than OS hotkey registration) needs no event loop on the
//! main thread, so the same code works on Linux (X11), macOS and Windows.

use std::{str::FromStr, time::Duration};

pub const POLL_INTERVAL: Duration = Duration::from_millis(30);

/// A key combination such as `ctrl+alt+r`. Every part must be held; each
/// part matches any of its keycodes (e.g. left or right Ctrl).
#[derive(Debug, Clone, PartialEq)]
pub struct Hotkey {
    parts: Vec<Vec<&'static str>>,
}

impl FromStr for Hotkey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts = s
            .split('+')
            .map(|p| keycodes(&p.trim().to_ascii_lowercase()))
            .collect::<Result<Vec<_>, _>>()?;
        if parts.is_empty() {
            return Err("empty hotkey".into());
        }
        Ok(Self { parts })
    }
}

/// Map one hotkey part to `device_query::Keycode` names.
fn keycodes(part: &str) -> Result<Vec<&'static str>, String> {
    const KEYS: &[&str] = &[
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
        "S", "T", "U", "V", "W", "X", "Y", "Z", "Key0", "Key1", "Key2", "Key3", "Key4", "Key5",
        "Key6", "Key7", "Key8", "Key9", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9",
        "F10", "F11", "F12", "Space", "Escape",
    ];
    Ok(match part {
        "ctrl" | "control" => vec!["LControl", "RControl"],
        "shift" => vec!["LShift", "RShift"],
        "alt" | "option" | "opt" => vec!["LAlt", "RAlt", "LOption", "ROption"],
        "cmd" | "command" | "meta" | "super" | "win" => vec!["LMeta", "RMeta", "Command"],
        "space" => vec!["Space"],
        "esc" | "escape" => vec!["Escape"],
        p => {
            let name = if p.len() == 1 && p.as_bytes()[0].is_ascii_digit() {
                format!("Key{p}")
            } else {
                p.to_ascii_uppercase()
            };
            let key = KEYS
                .iter()
                .find(|k| **k == name)
                .ok_or_else(|| format!("unknown key {p:?} in hotkey"))?;
            vec![key]
        }
    })
}

impl Hotkey {
    /// Whether the combination is held, given the names of all keys down.
    pub fn held<S: AsRef<str>>(&self, down: &[S]) -> bool {
        self.parts
            .iter()
            .all(|alts| down.iter().any(|d| alts.contains(&d.as_ref())))
    }
}

/// Turns "is the combo held" samples into one event per press.
#[derive(Default)]
pub struct EdgeDetector {
    was_held: bool,
}

impl EdgeDetector {
    pub fn pressed(&mut self, held: bool) -> bool {
        let edge = held && !self.was_held;
        self.was_held = held;
        edge
    }
}

#[cfg(feature = "input")]
pub fn keyboard() -> Option<impl FnMut() -> Vec<String>> {
    use device_query::{DeviceQuery, DeviceState};
    let state = DeviceState::checked_new()?;
    Some(move || state.get_keys().iter().map(|k| k.to_string()).collect())
}

#[cfg(not(feature = "input"))]
pub fn keyboard() -> Option<impl FnMut() -> Vec<String>> {
    None::<fn() -> Vec<String>>
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_matches() {
        let hk: Hotkey = "Ctrl+Alt+R".parse().unwrap();
        assert!(hk.held(&["LControl", "RAlt", "R"]));
        assert!(!hk.held(&["LControl", "R"]));
        let hk: Hotkey = "cmd+shift+5".parse().unwrap();
        assert!(hk.held(&["Command", "LShift", "Key5"]));
        assert!("ctrl+banana".parse::<Hotkey>().is_err());
    }

    #[test]
    fn fires_once_per_press() {
        let mut e = EdgeDetector::default();
        let events: Vec<_> = [false, true, true, false, true]
            .map(|h| e.pressed(h))
            .into();
        assert_eq!(events, [false, true, false, false, true]);
    }
}
