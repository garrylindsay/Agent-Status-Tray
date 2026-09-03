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

/// How the session list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    /// What needs you first, and within that the one that has been stuck longest.
    Attention,
    /// Most recent activity first, whatever state it is in.
    Recent,
    /// Oldest activity first.
    Oldest,
    /// Whatever is closest to losing its cached context first.
    GoingCold,
}

impl Sort {
    pub const ALL: [Sort; 4] = [
        Sort::Attention,
        Sort::Recent,
        Sort::Oldest,
        Sort::GoingCold,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Sort::Attention => "Attention first",
            Sort::Recent => "Most recent",
            Sort::Oldest => "Oldest",
            Sort::GoingCold => "Going cold first",
        }
    }
}

/// Which slice of a conversation the cost on a row covers.
///
/// Claude's own usage report totals the run you are in, not the whole conversation, so a session
/// resumed over several days reads far cheaper there than its transcript says it has cost. Both
/// are true; they answer different questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CostScope {
    /// Since the last long gap, which is what Claude's usage report shows.
    Run,
    /// Everything the conversation has ever cost, across every resume.
    Conversation,
    /// No cost column at all.
    Off,
}

impl CostScope {
    pub const ALL: [CostScope; 3] = [CostScope::Run, CostScope::Conversation, CostScope::Off];

    pub fn label(self) -> &'static str {
        match self {
            CostScope::Run => "This run",
            CostScope::Conversation => "Whole conversation",
            CostScope::Off => "Off",
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

/// Minutes a session's context is assumed to stay cached. `0` turns the countdown off.
///
/// This is your figure, not one Claude Code publishes: nothing on disk says when a session's
/// prompt cache expires. Transcripts here show a gap of forty minutes or more costing a rewrite of
/// the whole conversation, so an hour is a reasonable starting guess — but it is a guess, and the
/// row says "cold in" rather than anything more certain.
pub const CACHE_CHOICES: [u64; 5] = [0, 30, 60, 120, 240];

pub fn cache_label(mins: u64) -> String {
    match mins {
        0 => "Off".to_string(),
        60 => "1 hour".to_string(),
        120 => "2 hours".to_string(),
        240 => "4 hours".to_string(),
        n => format!("{n} minutes"),
    }
}

/// Session rows the tray menu will show. Capped rather than unbounded: a menu of every chat you
/// have ever had is unusable, and the sort puts what matters at the top anyway.
pub const LIST_CHOICES: [u64; 6] = [10, 15, 20, 30, 40, 50];

/// How far back chats that are not running are listed, in days. `0` leaves them out.
pub const CURSOR_LOCAL_CHOICES: [u64; 6] = [0, 1, 3, 7, 30, 90];

/// Same windows for Claude conversations whose process has exited.
pub const PAST_CHOICES: [u64; 6] = [0, 1, 3, 7, 30, 90];

pub fn past_label(days: u64) -> String {
    match days {
        0 => "Running only".to_string(),
        1 => "Last day".to_string(),
        7 => "Last week".to_string(),
        30 => "Last month".to_string(),
        n => format!("Last {n} days"),
    }
}

pub fn cursor_local_label(days: u64) -> String {
    match days {
        0 => "Cloud agents only".to_string(),
        1 => "Last day".to_string(),
        7 => "Last week".to_string(),
        30 => "Last month".to_string(),
        n => format!("Last {n} days"),
    }
}

/// Rows an alert will show before collapsing the rest into "+N more".
///
/// There is deliberately no "all of them": with a month of Cursor chats listed that is sixty-odd
/// rows, and an alert taller than the screen is not an alert.
pub const ROW_CHOICES: [u64; 8] = [3, 4, 5, 6, 8, 12, 20, 50];

pub fn rows_label(rows: u64) -> String {
    match rows {
        1 => "1 row".to_string(),
        n => format!("{n} rows"),
    }
}

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
    /// Order of the session list, in the menu and in alerts alike.
    pub sort: Sort,
    /// Rows an alert shows before the rest collapse into "+N more". `0` shows every one.
    pub max_alert_rows: u64,
    /// How far back local Cursor chats are listed, in days. `0` lists only cloud agents.
    pub cursor_local_days: u64,
    /// Session rows the tray menu shows before collapsing the rest into "+N more".
    pub max_list_rows: u64,
    /// How far back Claude conversations whose process has exited are listed, in days. `0` lists
    /// only sessions that are still running.
    pub claude_past_days: u64,
    /// Minutes a session's context is assumed to stay cached, for the countdown on each row.
    /// `0` leaves the countdown off.
    pub cache_window_mins: u64,
    /// How much of a conversation the cost on a row adds up.
    pub cost_scope: CostScope,
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
            sort: Sort::Attention,
            max_alert_rows: 4,
            cursor_local_days: 7,
            max_list_rows: 20,
            claude_past_days: 1,
            cache_window_mins: 60,
            // Matches what Claude's own usage report says, which is the number to hand.
            cost_scope: CostScope::Run,
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
        // Fifty is the ceiling here as well. A config written before the ceiling existed can hold
        // 0, which used to mean "all of them"; it becomes the ceiling rather than everything.
        if self.max_alert_rows == 0 {
            self.max_alert_rows = 50;
        }
        self.max_alert_rows = self.max_alert_rows.clamp(1, 50);
        // Fifty is the ceiling: past that the menu is taller than the screen and unusable.
        self.max_list_rows = self.max_list_rows.clamp(5, 50);
        // Neither of these has a live state, so a long window is all cost and no news.
        if self.cursor_local_days != 0 {
            self.cursor_local_days = self.cursor_local_days.clamp(1, 365);
        }
        if self.claude_past_days != 0 {
            self.claude_past_days = self.claude_past_days.clamp(1, 365);
        }
        if self.cache_window_mins != 0 {
            self.cache_window_mins = self.cache_window_mins.clamp(1, 1_440);
        }
        self.notify_statuses.retain(|s| NOTIFIABLE.contains(s));
        self.notify_statuses.dedup();
    }

    pub fn load() -> Config {
        let mut config = path()
            .and_then(|p| fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str::<Config>(strip_bom(&text)).ok())
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

/// Drops a leading byte-order mark.
///
/// This file is meant to be hand-editable, and plenty of Windows editors — Notepad, and PowerShell's
/// `Set-Content -Encoding utf8` — write one. JSON has no place for it, so `serde_json` rejects the
/// whole document and every setting silently reverts to its default, which is a miserable way to
/// find out.
fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
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
    fn the_default_order_is_attention_first() {
        assert_eq!(Config::default().sort, Sort::Attention);
    }

    #[test]
    fn round_trips_through_json() {
        let mut c = Config::default();
        c.sort = Sort::Recent;
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
            max_alert_rows: 9_999,
            max_list_rows: 9_999,
            ..Config::default()
        };
        c.sanitize();
        assert_eq!(c.poll_ms, 200);
        assert_eq!(c.repeat_secs, 5);
        assert_eq!(c.popup_secs, 3_600);
        assert_eq!(c.max_alert_rows, 50);
        assert_eq!(c.max_list_rows, 50);
    }

    /// A config written when zero meant "all of them" must land on the ceiling, not on one row.
    #[test]
    fn the_old_unbounded_setting_becomes_the_ceiling() {
        let mut c = Config {
            max_alert_rows: 0,
            ..Config::default()
        };
        c.sanitize();
        assert_eq!(c.max_alert_rows, 50);
    }

    /// A BOM must not throw away every setting in the file.
    #[test]
    fn a_byte_order_mark_is_tolerated() {
        let text = "\u{feff}{\"pollMs\":2000,\"sort\":\"recent\"}";
        let parsed: Config = serde_json::from_str(strip_bom(text)).unwrap();
        assert_eq!(parsed.poll_ms, 2_000);
        assert_eq!(parsed.sort, Sort::Recent);
        // And the same document without one still parses.
        assert!(serde_json::from_str::<Config>(strip_bom("{\"pollMs\":2000}")).is_ok());
    }

    /// Missing keys fall back to defaults rather than dropping the file.
    #[test]
    fn partial_json_loads() {
        let c: Config = serde_json::from_str(r#"{"pollMs":2000}"#).unwrap();
        assert_eq!(c.poll_ms, 2_000);
        assert_eq!(c.sound, Sound::Notification);
    }
}
