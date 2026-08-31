//! Reads Claude Code's on-disk session registry.
//!
//! Claude Code writes one file per running session to `%USERPROFILE%\.claude\sessions\<pid>.json`
//! and rewrites it on every status change. Fields used here come from that file; everything else
//! in it is ignored.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::liveness;

/// Serialized in the config file as lowercase names, matching the on-disk registry vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Waiting,
    Busy,
    Shell,
    Idle,
    Unknown,
}

impl Status {
    fn parse(raw: &str) -> Status {
        match raw {
            "waiting" => Status::Waiting,
            "busy" => Status::Busy,
            "shell" => Status::Shell,
            "idle" => Status::Idle,
            _ => Status::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Waiting => "WAITING",
            Status::Busy => "BUSY",
            Status::Shell => "SHELL",
            Status::Idle => "IDLE",
            Status::Unknown => "?",
        }
    }

    /// Longer text for the settings panel, where `WAITING` alone reads as shouting.
    pub fn menu_label(self) -> &'static str {
        match self {
            Status::Waiting => "Waiting on you",
            Status::Busy => "Busy",
            Status::Shell => "Running a shell command",
            Status::Idle => "Idle",
            Status::Unknown => "Unknown / not reported",
        }
    }

    /// Sort key: attention-needing sessions first.
    fn rank(self) -> u8 {
        match self {
            Status::Waiting => 0,
            Status::Busy | Status::Shell => 1,
            Status::Idle => 2,
            Status::Unknown => 3,
        }
    }
}

/// The subset of the on-disk record this program renders.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Raw {
    cwd: String,
    name: Option<String>,
    session_id: Option<String>,
    /// `interactive`, `claude-desktop`, ... — names what is hosting the session.
    entrypoint: Option<String>,
    status: String,
    waiting_for: Option<String>,
    kind: Option<String>,
    started_at: Option<u64>,
    status_updated_at: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub pid: u32,
    pub name: String,
    /// Working directory, which locates the transcript holding the conversation title.
    pub cwd: String,
    /// Conversation title, when one has been set or generated for the session.
    pub title: Option<String>,
    /// Claude Code's own id for the session, used to build a deep link.
    pub session_id: Option<String>,
    pub entrypoint: Option<String>,
    pub status: Status,
    pub waiting_for: Option<String>,
    /// Epoch ms the current status was entered; falls back to session start.
    pub since: u64,
}

impl Session {
    /// Deep link that opens this exact session in the Claude desktop app.
    ///
    /// The app's handler validates `session` against `^local_[A-Za-z0-9-]{1,64}$` and matches it
    /// to its own store, where ids are minted as `local_<uuid>` — the uuid being the `sessionId`
    /// in the registry file. Only offered for sessions the desktop app is hosting: firing
    /// `claude://` for a session running in a terminal would raise the wrong application.
    ///
    /// As of Claude desktop 1.40609.0 the handler is behind a server-side feature flag and logs
    /// `code entry deep link gated off`, so this currently resolves to nothing happening. It costs
    /// one no-op launch per click and starts working by itself once that flag is enabled.
    pub fn deep_link(&self) -> Option<String> {
        if self.entrypoint.as_deref() != Some("claude-desktop") {
            return None;
        }
        let id = self.session_id.as_deref()?;
        if id.is_empty()
            || id.len() > 57
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return None;
        }
        Some(format!(
            "claude://code/continue?session=local_{id}&source=desktop_action"
        ))
    }
}

/// `%USERPROFILE%\.claude\sessions`, honoring `CLAUDE_CONFIG_DIR` when set.
pub fn sessions_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir).join("sessions"));
        }
    }
    let home = std::env::var("USERPROFILE")
        .ok()
        .filter(|h| !h.trim().is_empty())?;
    Some(PathBuf::from(home).join(".claude").join("sessions"))
}

fn pid_from_file_name(name: &str) -> Option<u32> {
    let stem = name.strip_suffix(".json")?;
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

fn display_name(pid: u32, raw: &Raw) -> String {
    if let Some(name) = raw.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    let leaf = raw
        .cwd
        .trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or("")
        .trim();
    if leaf.is_empty() {
        format!("pid {pid}")
    } else {
        leaf.to_string()
    }
}

/// Reader for the session registry.
///
/// Holds the last successfully parsed record per pid: Claude Code rewrites these files in place,
/// so a tick can catch one mid-write. Falling back to the cached copy keeps a row from flickering
/// out of the list for a tick.
#[derive(Default)]
pub struct Registry {
    cache: HashMap<u32, Raw>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry::default()
    }

    /// Scan the registry and return live sessions, attention-needing ones first.
    pub fn scan(&mut self) -> Vec<Session> {
        match sessions_dir() {
            Some(dir) => self.scan_dir(&dir),
            None => Vec::new(),
        }
    }

    fn scan_dir(&mut self, dir: &Path) -> Vec<Session> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };

        let mut sessions = Vec::new();
        let mut seen = Vec::new();

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let Some(pid) = file_name.to_str().and_then(pid_from_file_name) else {
                continue;
            };

            if !liveness::is_claude_process(pid) {
                self.cache.remove(&pid);
                continue;
            }
            seen.push(pid);

            let parsed = fs::read_to_string(entry.path())
                .ok()
                .and_then(|text| serde_json::from_str::<Raw>(&text).ok());

            let raw = match parsed {
                Some(raw) => {
                    self.cache.insert(pid, raw.clone());
                    raw
                }
                None => match self.cache.get(&pid) {
                    Some(raw) => raw.clone(),
                    None => continue,
                },
            };

            // Daemon processes are infrastructure, not conversations — never worth a row.
            if matches!(raw.kind.as_deref(), Some("daemon") | Some("daemon-worker")) {
                continue;
            }

            sessions.push(Session {
                pid,
                name: display_name(pid, &raw),
                cwd: raw.cwd.clone(),
                title: None,
                session_id: raw
                    .session_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string),
                entrypoint: raw.entrypoint.clone(),
                status: Status::parse(&raw.status),
                waiting_for: raw
                    .waiting_for
                    .as_deref()
                    .map(str::trim)
                    .filter(|w| !w.is_empty())
                    .map(str::to_string),
                since: raw.status_updated_at.or(raw.started_at).unwrap_or(0),
            });
        }

        self.cache.retain(|pid, _| seen.contains(pid));

        // Attention first, then longest-standing within a status.
        sessions.sort_by(|a, b| {
            a.status
                .rank()
                .cmp(&b.status.rank())
                .then(a.since.cmp(&b.since))
                .then(a.pid.cmp(&b.pid))
        });
        sessions
    }
}

/// What the tray icon should show for a set of sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IconState {
    pub kind: IconKind,
    pub count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Waiting,
    Busy,
    Idle,
    Empty,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_names() {
        assert_eq!(pid_from_file_name("39560.json"), Some(39560));
        assert_eq!(pid_from_file_name("pins.json"), None);
        assert_eq!(pid_from_file_name("39560.json.tmp"), None);
        assert_eq!(pid_from_file_name(".json"), None);
    }

    /// A killed session leaves its file behind; it must not produce a row.
    #[test]
    fn stale_and_non_session_files_are_ignored() {
        let dir = std::env::temp_dir().join("claude-tray-test-registry");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // A pid this high is not going to be running.
        let dead_pid = u32::MAX - 1;
        fs::write(
            dir.join(format!("{dead_pid}.json")),
            r#"{"pid":4294967294,"cwd":"C:\\repos\\x","name":"ghost","status":"waiting",
                "waitingFor":"permission prompt","kind":"interactive","statusUpdatedAt":1}"#,
        )
        .unwrap();
        fs::write(dir.join("pins.json"), "{}").unwrap();

        let sessions = Registry::new().scan_dir(&dir);
        assert!(sessions.is_empty(), "got rows for dead sessions: {sessions:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// The image-name check is what stops a recycled pid from resurrecting a session.
    #[test]
    fn liveness_requires_a_claude_process() {
        assert!(!liveness::is_claude_process(u32::MAX - 1));
        // This test binary is alive but is not claude.exe.
        assert!(!liveness::is_claude_process(std::process::id()));
    }

    fn session(entrypoint: Option<&str>, session_id: Option<&str>) -> Session {
        Session {
            pid: 1,
            name: "x".to_string(),
            cwd: String::new(),
            title: None,
            session_id: session_id.map(str::to_string),
            entrypoint: entrypoint.map(str::to_string),
            status: Status::Idle,
            waiting_for: None,
            since: 0,
        }
    }

    /// The `local_` prefix and the exact query shape are what the desktop app validates.
    #[test]
    fn deep_link_matches_what_the_app_accepts() {
        let s = session(
            Some("claude-desktop"),
            Some("48a3f9c8-1785-4075-a0e0-e46e97574e8b"),
        );
        assert_eq!(
            s.deep_link().as_deref(),
            Some(
                "claude://code/continue?session=local_48a3f9c8-1785-4075-a0e0-e46e97574e8b\
                 &source=desktop_action"
            )
        );
    }

    /// Firing `claude://` for a terminal-hosted session would raise the wrong application.
    #[test]
    fn only_desktop_hosted_sessions_get_a_deep_link() {
        assert!(session(Some("interactive"), Some("abc")).deep_link().is_none());
        assert!(session(None, Some("abc")).deep_link().is_none());
        assert!(session(Some("claude-desktop"), None).deep_link().is_none());
    }

    /// Anything the app's regex would reject is not worth launching a process for.
    #[test]
    fn ids_that_would_fail_validation_are_refused() {
        assert!(session(Some("claude-desktop"), Some("")).deep_link().is_none());
        assert!(
            session(Some("claude-desktop"), Some("has space"))
                .deep_link()
                .is_none()
        );
        assert!(
            session(Some("claude-desktop"), Some("semi;colon"))
                .deep_link()
                .is_none()
        );
        // `local_` plus this is 65 characters, one past what the app allows.
        let too_long = "a".repeat(58);
        assert!(
            session(Some("claude-desktop"), Some(&too_long))
                .deep_link()
                .is_none()
        );
    }

    /// Prints what the tray would show for the sessions running right now.
    /// `cargo test -- --nocapture live_registry`
    #[test]
    fn live_registry() {
        let sessions = Registry::new().scan();
        println!("{:?}", sessions_dir());
        for s in &sessions {
            println!(
                "pid={} status={:?} waiting_for={:?} name={}",
                s.pid, s.status, s.waiting_for, s.name
            );
        }
        println!("icon: {:?}", icon_state(&sessions));
    }
}

pub fn icon_state(sessions: &[Session]) -> IconState {
    let waiting = sessions
        .iter()
        .filter(|s| s.status == Status::Waiting)
        .count();
    let busy = sessions
        .iter()
        .filter(|s| matches!(s.status, Status::Busy | Status::Shell))
        .count();

    if waiting > 0 {
        IconState {
            kind: IconKind::Waiting,
            count: waiting,
        }
    } else if busy > 0 {
        IconState {
            kind: IconKind::Busy,
            count: busy,
        }
    } else if !sessions.is_empty() {
        IconState {
            kind: IconKind::Idle,
            count: sessions.len(),
        }
    } else {
        IconState {
            kind: IconKind::Empty,
            count: 0,
        }
    }
}
