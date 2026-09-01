//! The Claude desktop app's own record of each session.
//!
//! The session registry says a session exists; this says what has happened to it. The desktop app
//! keeps one JSON file per session under
//! `%APPDATA%\Claude\claude-code-sessions\<workspace>\<project>\local_<uuid>.json`, and keeps it
//! current — unlike the registry file, which is written once at session start.
//!
//! Two fields matter here. `lastActivityAt` against `lastFocusedAt` is what the desktop app's blue
//! dot means: something happened in that session since you last looked at it. And `sessionId` is
//! the id its deep links expect, which is *not* `local_` plus the CLI session id — the two are
//! different uuids, related only by `cliSessionId`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::session::Repo;

/// The fields used here. The files also carry MCP tool lists and other bulk that is ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    session_id: String,
    cli_session_id: Option<String>,
    last_focused_at: Option<u64>,
    last_activity_at: Option<u64>,
    is_archived: Option<bool>,
    title: Option<String>,
    cwd: Option<String>,
    /// Branches this session wrote, whether or not a pull request came of them.
    written_branches: Option<Vec<String>>,
    prs: Option<Vec<Pr>>,
}

/// A pull request the session opened. Only its state is used here.
#[derive(Debug, Clone, Deserialize)]
struct Pr {
    state: Option<String>,
}

/// What the tray takes from a desktop record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// The id the app's `claude://` deep links expect.
    pub session_id: String,
    pub last_focused_at: u64,
    pub last_activity_at: u64,
    pub archived: bool,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub repo: Repo,
}

impl Info {
    /// True when something happened after you last looked — the desktop app's blue dot.
    pub fn unread(&self) -> bool {
        self.last_activity_at > self.last_focused_at
    }
}

impl Record {
    /// The session's repository state, most-live first.
    ///
    /// A session can hold several pull requests at once — one merged, one still open. The open one
    /// is the one still wanting something, so it decides the mark.
    fn repo(&self) -> Repo {
        let prs = self.prs.as_deref().unwrap_or(&[]);
        let state = |want: &str| {
            prs.iter()
                .any(|pr| pr.state.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(want)))
        };
        if state("OPEN") || state("DRAFT") {
            return Repo::PrOpen;
        }
        if state("MERGED") {
            return Repo::PrMerged;
        }
        // A branch with no pull request at all is work that has not been put up yet.
        let branches = self.written_branches.as_deref().unwrap_or(&[]);
        if branches.iter().any(|b| !b.trim().is_empty()) {
            return Repo::Branch;
        }
        Repo::Nothing
    }
}

/// Folder leaf for a path, which is what a row shows as its name.
fn folder_leaf(path: &str) -> Option<String> {
    let leaf = path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next()?;
    (!leaf.trim().is_empty()).then(|| leaf.to_string())
}

/// `%APPDATA%\Claude\claude-code-sessions`.
fn store_dir() -> Option<PathBuf> {
    let base = std::env::var("APPDATA")
        .ok()
        .filter(|d| !d.trim().is_empty())?;
    Some(PathBuf::from(base).join("Claude").join("claude-code-sessions"))
}

/// Records are nested a couple of levels under the store, so the walk is bounded rather than
/// unlimited: a deep recursive scan on every tick would not be worth the state it returns.
const MAX_DEPTH: usize = 4;

fn collect(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            collect(&entry.path(), depth + 1, out);
        } else if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("local_") && n.ends_with(".json"))
        {
            // Modified time doubles as the cache key: an unchanged file is never re-parsed.
            let stamp = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            out.push((entry.path(), stamp));
        }
    }
}

#[derive(Default)]
pub struct Desktop {
    /// Parsed record per file, keyed by path, with the stamp it was parsed at.
    parsed: HashMap<PathBuf, (u64, Option<Record>)>,
    /// Lookup by CLI session id, which is what the registry gives us.
    by_cli: HashMap<String, Info>,
}

impl Desktop {
    pub fn new() -> Desktop {
        Desktop::default()
    }

    /// Re-reads any record that changed since the last call.
    pub fn refresh(&mut self) {
        let Some(dir) = store_dir() else { return };
        let mut files = Vec::new();
        collect(&dir, 0, &mut files);

        let present: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
        self.parsed.retain(|path, _| present.contains(path));

        for (path, stamp) in files {
            let fresh = match self.parsed.get(&path) {
                Some((seen, _)) if *seen == stamp => continue,
                _ => fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Record>(&text).ok()),
            };
            self.parsed.insert(path, (stamp, fresh));
        }

        self.by_cli.clear();
        for (_, record) in self.parsed.values() {
            let Some(record) = record else { continue };
            let Some(cli) = record.cli_session_id.clone() else {
                continue;
            };
            self.by_cli.insert(
                cli,
                Info {
                    session_id: record.session_id.clone(),
                    last_focused_at: record.last_focused_at.unwrap_or(0),
                    last_activity_at: record.last_activity_at.unwrap_or(0),
                    archived: record.is_archived.unwrap_or(false),
                    cwd: record.cwd.clone(),
                    repo: record.repo(),
                    title: record
                        .title
                        .clone()
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty()),
                },
            );
        }
    }

    pub fn get(&self, cli_session_id: &str) -> Option<&Info> {
        self.by_cli.get(cli_session_id)
    }

    /// Conversations that are not running any more, within `days`.
    ///
    /// A Claude Code session leaves the registry the moment its process exits, but the
    /// conversation does not go anywhere: it stays in the app's list, still unread, still wanting
    /// a reply. Listing only live processes made those disappear from the tray while Cursor's
    /// finished chats stayed — the same information, treated differently for no reason other than
    /// where it happened to be stored.
    pub fn past_sessions(
        &self,
        live: &[String],
        days: u64,
        now_ms: u64,
        pid: u32,
    ) -> Vec<crate::session::Session> {
        use crate::session::{Provider, Session, Status};

        if days == 0 {
            return Vec::new();
        }
        let cutoff = now_ms.saturating_sub(days.saturating_mul(86_400_000));

        self.by_cli
            .iter()
            .filter(|(cli, info)| {
                !info.archived && info.last_activity_at >= cutoff && !live.iter().any(|l| l == *cli)
            })
            .map(|(cli, info)| Session {
                provider: Provider::ClaudeCode,
                // No process of its own any more, so clicking raises the app that holds it.
                pid,
                name: info
                    .cwd
                    .as_deref()
                    .and_then(folder_leaf)
                    .unwrap_or_else(|| "claude".to_string()),
                cwd: info.cwd.clone().unwrap_or_default(),
                title: info.title.clone(),
                session_id: Some(cli.clone()),
                entrypoint: Some("claude-desktop".to_string()),
                desktop_session_id: Some(info.session_id.clone()),
                repo: info.repo,
                // Nothing is running, so the only thing left to say is whether it was read.
                status: if info.unread() {
                    Status::Unread
                } else {
                    Status::Idle
                },
                waiting_for: None,
                since: info.last_activity_at,
            })
            .collect()
    }

    /// Fills in what the registry cannot say: the deep-link id, and the finished/unread state.
    ///
    /// A status the registry actually reported always wins — this only speaks where the registry
    /// is silent, which on builds that never write `status` is everywhere. `lastActivityAt`
    /// becomes the row's timestamp, so the elapsed time is "finished 3m ago" rather than the
    /// session's age.
    pub fn apply(&mut self, sessions: &mut [crate::session::Session]) {
        use crate::session::Status;

        self.refresh();
        for session in sessions {
            let Some(id) = session.session_id.clone() else {
                continue;
            };
            let Some(info) = self.get(&id) else { continue };

            session.desktop_session_id = Some(info.session_id.clone());
            session.repo = info.repo;
            if session.title.is_none() {
                session.title = info.title.clone();
            }

            if session.status == Status::Unknown {
                session.status = if info.unread() {
                    Status::Unread
                } else {
                    Status::Idle
                };
                if info.last_activity_at > 0 {
                    session.since = info.last_activity_at;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(focused: u64, activity: u64) -> Info {
        Info {
            session_id: "local_x".to_string(),
            last_focused_at: focused,
            last_activity_at: activity,
            archived: false,
            title: None,
            cwd: None,
            repo: Repo::Nothing,
        }
    }

    fn record(states: &[&str], branches: &[&str]) -> Record {
        Record {
            session_id: "local_x".to_string(),
            cli_session_id: None,
            last_focused_at: None,
            last_activity_at: None,
            is_archived: None,
            title: None,
            cwd: None,
            written_branches: Some(branches.iter().map(|b| b.to_string()).collect()),
            prs: Some(
                states
                    .iter()
                    .map(|s| Pr {
                        state: Some(s.to_string()),
                    })
                    .collect(),
            ),
        }
    }

    /// An open pull request outranks a merged one: it is the one still wanting something.
    #[test]
    fn an_open_pull_request_wins() {
        assert_eq!(record(&["MERGED", "OPEN"], &["a"]).repo(), Repo::PrOpen);
        assert_eq!(record(&["OPEN"], &[]).repo(), Repo::PrOpen);
        assert_eq!(record(&["DRAFT"], &[]).repo(), Repo::PrOpen);
    }

    #[test]
    fn merged_beats_a_bare_branch() {
        assert_eq!(record(&["MERGED"], &["a"]).repo(), Repo::PrMerged);
    }

    /// A branch with no pull request is work that has not been put up yet.
    #[test]
    fn a_branch_without_a_pull_request_is_its_own_state() {
        assert_eq!(record(&[], &["feat/x"]).repo(), Repo::Branch);
        assert_eq!(record(&[], &[]).repo(), Repo::Nothing);
        assert_eq!(record(&[], &["  "]).repo(), Repo::Nothing);
    }

    /// Closed-without-merging is neither live nor done; the branch is what remains.
    #[test]
    fn a_closed_pull_request_falls_back_to_the_branch() {
        assert_eq!(record(&["CLOSED"], &["feat/x"]).repo(), Repo::Branch);
        assert_eq!(record(&["CLOSED"], &[]).repo(), Repo::Nothing);
    }

    /// Blue means "something happened since you looked", not "something happened".
    #[test]
    fn unread_is_activity_after_the_last_look() {
        assert!(info(100, 200).unread());
        assert!(!info(200, 100).unread());
        // Looked at exactly as it finished: nothing left to read.
        assert!(!info(200, 200).unread());
    }

    /// A record that has never been focused still counts as unread once anything happens.
    #[test]
    fn a_never_focused_session_with_activity_is_unread() {
        assert!(info(0, 1).unread());
        assert!(!info(0, 0).unread());
    }

    /// Reads this machine's real records.
    /// `cargo test -- --nocapture live_desktop_records`
    #[test]
    fn live_desktop_records() {
        let mut desktop = Desktop::new();
        desktop.refresh();
        for session in crate::session::Registry::new().scan() {
            let info = session.session_id.as_deref().and_then(|id| desktop.get(id));
            match info {
                Some(info) => println!(
                    "{} -> unread={} id={} title={:?}",
                    session.name,
                    info.unread(),
                    info.session_id,
                    info.title
                ),
                None => println!("{} -> no desktop record", session.name),
            }
        }
    }
}
