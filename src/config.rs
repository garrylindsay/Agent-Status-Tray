//! Persisted user settings.
//!
//! Everything configurable from the tray menu lives here and is written to
//! `%APPDATA%\claude-tray\config.json` on every change, so settings survive a reboot.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::session::Status;

/// Sounds offered alongside a popup. These are Win32 event aliases, not files, so there is
/// nothing to ship and they follow whatever sound scheme the user has selected in Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sound {
    None,
    /// `MessageBeep(MB_OK)` — the plain system beep.
    Default,
    Asterisk,
    Exclamation,
    Hand,
    Question,
    /// Windows 10/11 notification chime, falling back to Asterisk where the alias is unknown.
    Notification,
}

impl Sound {
    pub const ALL: [Sound; 7] = [
        Sound::None,
        Sound::Default,
        Sound::Notification,
        Sound::Asterisk,
        Sound::Exclamation,
        Sound::Hand,
        Sound::Question,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Sound::None => "No sound",
            Sound::Default => "System beep",
            Sound::Notification => "Notification",
            Sound::Asterisk => "Asterisk",
            Sound::Exclamation => "Exclamation",
            Sound::Hand => "Critical stop",
            Sound::Question => "Question",
        }
    }

}

/// Statuses that can be chosen as notification triggers, in menu order.
pub const NOTIFIABLE: [Status; 7] = [
    Status::Waiting,
    Status::Error,
    Status::Busy,
    Status::Shell,
    Status::Unread,
    Status::Idle,
    Status::Unknown,
];

/// Repeat intervals offered in the menu, in seconds. `0` means "notify once, never repeat".
pub const REPEAT_CHOICES: [u64; 8] = [0, 30, 60, 120, 300, 600, 900, 1800];

/// Poll intervals offered in the menu, in milliseconds.
pub const POLL_CHOICES: [u64; 6] = [500, 1_000, 2_000, 5_000, 10_000, 30_000];

/// How long a popup stays on screen, in seconds. `0` means "stay until clicked".
pub const POPUP_CHOICES: [u64; 5] = [0, 5, 8, 15, 30];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// Master switch for popups.
    pub notifications_enabled: bool,
    /// Sessions in any of these statuses raise a popup.
    pub notify_statuses: Vec<Status>,
    /// Seconds between repeat popups while a session stays in a watched status.
    pub repeat_secs: u64,
    /// How often the session registry is re-read, in milliseconds.
    pub poll_ms: u64,
    pub sound: Sound,
    /// Seconds before a popup dismisses itself.
    pub popup_secs: u64,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            notifications_enabled: true,
            notify_statuses: vec![Status::Waiting],
            repeat_secs: 60,
            poll_ms: 1_000,
            sound: Sound::Notification,
            popup_secs: 8,
        }
    }
}

impl Config {
    pub fn notifies_on(&self, status: Status) -> bool {
        self.notify_statuses.contains(&status)
    }

    pub fn toggle_status(&mut self, status: Status) {
        match self.notify_statuses.iter().position(|s| *s == status) {
            Some(i) => {
                self.notify_statuses.remove(i);
            }
            None => self.notify_statuses.push(status),
        }
    }

    /// Clamped on load so a hand-edited file cannot wedge the poll loop at 0ms.
    fn sanitize(&mut self) {
        self.poll_ms = self.poll_ms.clamp(200, 300_000);
        if self.repeat_secs != 0 {
            self.repeat_secs = self.repeat_secs.clamp(5, 86_400);
        }
        if self.popup_secs != 0 {
            self.popup_secs = self.popup_secs.clamp(2, 3_600);
        }
        self.notify_statuses.retain(|s| NOTIFIABLE.contains(s));
        self.notify_statuses.dedup();
    }

    pub fn load() -> Config {
        let mut config = path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str::<Config>(&text).ok())
            .unwrap_or_default();
        config.sanitize();
        config
    }

    /// Best effort: a settings write that fails must not take the tray down.
    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, text);
        }
    }
}

/// `%APPDATA%\claude-tray\config.json`, falling back to the profile directory.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var("APPDATA")
        .ok()
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|d| !d.trim().is_empty())
                .map(PathBuf::from)
        })?;
    Some(base.join("claude-tray").join("config.json"))
}

/// Human text for a repeat interval.
pub fn repeat_label(secs: u64) -> String {
    match secs {
        0 => "Only once".to_string(),
        s if s < 60 => format!("Every {s} seconds"),
        60 => "Every minute".to_string(),
        s => format!("Every {} minutes", s / 60),
    }
}

pub fn poll_label(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        let s = ms / 1000;
        if s == 1 {
            "1 second".to_string()
        } else {
            format!("{s} seconds")
        }
    } else {
        format!("{:.1} seconds", ms as f64 / 1000.0)
    }
}

pub fn popup_label(secs: u64) -> String {
    match secs {
        0 => "Until clicked".to_string(),
        s => format!("{s} seconds"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_documented_behaviour() {
        let c = Config::default();
        assert_eq!(c.poll_ms, 1_000);
        assert_eq!(c.repeat_secs, 60);
        assert!(c.notifies_on(Status::Waiting));
        assert!(!c.notifies_on(Status::Idle));
    }

    #[test]
    fn round_trips_through_json() {
        let mut c = Config::default();
        c.toggle_status(Status::Idle);
        c.sound = Sound::Asterisk;
        c.poll_ms = 5_000;
        let text = serde_json::to_string(&c).unwrap();
        let back: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn toggling_a_status_adds_then_removes_it() {
        let mut c = Config::default();
        c.toggle_status(Status::Busy);
        assert!(c.notifies_on(Status::Busy));
        c.toggle_status(Status::Busy);
        assert!(!c.notifies_on(Status::Busy));
    }

    /// A hand-edited config must not be able to spin the poll loop.
    #[test]
    fn absurd_values_are_clamped() {
        let mut c = Config {
            poll_ms: 0,
            repeat_secs: 1,
            popup_secs: 99_999,
            ..Config::default()
        };
        c.sanitize();
        assert_eq!(c.poll_ms, 200);
        assert_eq!(c.repeat_secs, 5);
        assert_eq!(c.popup_secs, 3_600);
    }

    /// Missing keys fall back to defaults rather than dropping the file.
    #[test]
    fn partial_json_loads() {
        let c: Config = serde_json::from_str(r#"{"pollMs":2000}"#).unwrap();
        assert_eq!(c.poll_ms, 2_000);
        assert_eq!(c.sound, Sound::Notification);
    }
}
