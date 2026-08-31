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

/// Which agent tool a session belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    ClaudeCode,
    Cursor,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::ClaudeCode => "Claude",
            Provider::Cursor => "Cursor",
        }
    }
}

/// Serialized in the config file as lowercase names, matching the on-disk registry vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Waiting,
    Busy,
    Shell,
    /// Finished, with something you have not looked at yet. Not a registry status: it comes from
    /// the desktop app's record of when you last focused the session.
    Unread,
    /// Ended badly and wants looking at. Reported by Cursor; Claude Code has no equivalent.
    Error,
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
            Status::Unread => "UNREAD",
            Status::Error => "ERROR",
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
            Status::Unread => "Finished, not looked at",
            Status::Error => "Failed",
            Status::Idle => "Finished",
            Status::Unknown => "Unknown / not reported",
        }
    }

    /// Sort key: attention-needing sessions first.
    pub fn rank(self) -> u8 {
        match self {
            Status::Waiting => 0,
            // A failure is as worth surfacing as a prompt, and more than work in progress.
            Status::Error => 1,
            Status::Busy | Status::Shell => 2,
            Status::Unread => 3,
            Status::Idle => 4,
            Status::Unknown => 5,
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
    pub provider: Provider,
    /// Process to raise when the row is clicked. Zero when the session has no process of its own.
    pub pid: u32,
    pub name: String,
    /// Working directory, which locates the transcript holding the conversation title.
    pub cwd: String,
    /// Conversation title, when one has been set or generated for the session.
    pub title: Option<String>,
    /// The desktop app's id for this session, when it has a record of it. Its deep links expect
    /// this, not the CLI session id.
    pub desktop_session_id: Option<String>,
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
    /// The app's handler validates `session` against `^local_[A-Za-z0-9-]{1,64}$` and then looks
    /// it up as `sessionId` in its own store. That id is **not** `local_` plus the CLI session id
    /// — the two are different uuids — so it has to come from the desktop record, and no link is
    /// offered without one. Only for sessions the desktop app is hosting: firing `claude://` for a
    /// session running in a terminal would raise the wrong application.
    ///
    /// As of Claude desktop 1.40609.0 the handler is behind a server-side feature flag and logs
    /// `code entry deep link gated off`, so this currently resolves to nothing happening. It costs
    /// one no-op launch per click and starts working by itself once that flag is enabled.
    pub fn deep_link(&self) -> Option<String> {
        if self.entrypoint.as_deref() != Some("claude-desktop") {
            return None;
        }
        let id = self.desktop_session_id.as_deref()?;
        let body = id.strip_prefix("local_")?;
        if body.is_empty()
            || body.len() > 64
            || !body.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return None;
        }
        Some(format!(
            "claude://code/continue?session={id}&source=desktop_action"
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
                provider: Provider::ClaudeCode,
                pid,
                name: display_name(pid, &raw),
                cwd: raw.cwd.clone(),
                title: None,
                desktop_session_id: None,
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

    fn session(entrypoint: Option<&str>, desktop_session_id: Option<&str>) -> Session {
        Session {
            provider: Provider::ClaudeCode,
            pid: 1,
            name: "x".to_string(),
            cwd: String::new(),
            title: None,
            desktop_session_id: desktop_session_id.map(str::to_string),
            session_id: Some("48a3f9c8-1785-4075-a0e0-e46e97574e8b".to_string()),
            entrypoint: entrypoint.map(str::to_string),
            status: Status::Idle,
            waiting_for: None,
            since: 0,
        }
    }

    /// The link carries the desktop app's own id verbatim. It is a different uuid from the CLI
    /// session id, so building `local_<cliSessionId>` would pass the app's format check and then
    /// match nothing in its store.
    #[test]
    fn deep_link_uses_the_desktop_id_not_the_cli_one() {
        let s = session(
            Some("claude-desktop"),
            Some("local_193507c2-d1ff-4a54-9d6b-c84a4dd1bb33"),
        );
        assert_eq!(
            s.deep_link().as_deref(),
            Some(
                "claude://code/continue?session=local_193507c2-d1ff-4a54-9d6b-c84a4dd1bb33\
                 &source=desktop_action"
            )
        );
        // Nothing is invented from the CLI id when the desktop record is missing.
        assert!(session(Some("claude-desktop"), None).deep_link().is_none());
    }

    /// Firing `claude://` for a terminal-hosted session would raise the wrong application.
    #[test]
    fn only_desktop_hosted_sessions_get_a_deep_link() {
        assert!(
            session(Some("interactive"), Some("local_abc"))
                .deep_link()
                .is_none()
        );
        assert!(session(None, Some("local_abc")).deep_link().is_none());
    }

    /// Anything the app's regex would reject is not worth launching a process for.
    #[test]
    fn ids_that_would_fail_validation_are_refused() {
        // Must carry the prefix the app's pattern requires.
        assert!(
            session(Some("claude-desktop"), Some("193507c2"))
                .deep_link()
                .is_none()
        );
        assert!(session(Some("claude-desktop"), Some("local_")).deep_link().is_none());
        assert!(
            session(Some("claude-desktop"), Some("local_has space"))
                .deep_link()
                .is_none()
        );
        assert!(
            session(Some("claude-desktop"), Some("local_semi;colon"))
                .deep_link()
                .is_none()
        );
        // One character past what the app allows after the prefix.
        let too_long = format!("local_{}", "a".repeat(65));
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
