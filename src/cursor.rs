//! Cursor's cloud (background) agents.
//!
//! Cursor keeps them in its VS Code-style key/value store at
//! `%APPDATA%\Cursor\User\globalStorage\state.vscdb`, under keys beginning
//! `cloudAgentRepository.agents.`. The value is a JSON array of agent records carrying a status, an
//! unread flag, and last-activity time — the same three things this tray wants.
//!
//! Local chats are read too, when the settings ask for them. They take three sources to assemble:
//! `conversation-search.db` lists them with a timestamp, `composerData:<id>` in the key/value
//! store holds the real title and how the chat ended, and `glass.localAgentProjects` with
//! `glass.localAgentProjectMembership` says which folder each belongs to. The search index alone
//! is not enough — it lags, and the newest chat is usually in it with an empty title.
//!
//! They are limited to a recent window because there are hundreds of them, and none reports a live
//! state: a local chat is only ever finished.
//!
//! Every store is opened read-only and never written to.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::session::{Provider, Session, Status};

/// `aiserver.v1.BackgroundComposerStatus`, read out of Cursor's own bundle rather than guessed.
mod status {
    pub const RUNNING: i64 = 1;
    pub const FINISHED: i64 = 2;
    pub const ERROR: i64 = 3;
    pub const CREATING: i64 = 4;
    pub const EXPIRED: i64 = 5;
}

/// Re-reading a 1.8GB store on every tick is not worth it: agents change on the order of seconds
/// at best, and the poll interval can be as low as half a second.
const MIN_RESCAN_MS: u64 = 3_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Agent {
    bc_id: Option<String>,
    name: Option<String>,
    status: Option<i64>,
    is_unread: Option<bool>,
    is_archived: Option<bool>,
    is_killed: Option<bool>,
    last_message_activity_at_ms: Option<u64>,
    updated_at: Option<u64>,
    repo_url: Option<String>,
    workspace_root_path: Option<String>,
}

impl Agent {
    /// What to show as the session's name: the repository it is working in.
    fn repo(&self) -> String {
        if let Some(url) = self.repo_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            let leaf = url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(url)
                .trim_end_matches(".git");
            if !leaf.is_empty() {
                return leaf.to_string();
            }
        }
        if let Some(path) = self
            .workspace_root_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let leaf = path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next();
            if let Some(leaf) = leaf.filter(|l| !l.is_empty()) {
                return leaf.to_string();
            }
        }
        "cursor".to_string()
    }

    /// Cursor's status, plus its unread flag, mapped onto this tray's states.
    fn status(&self) -> Status {
        match self.status.unwrap_or(0) {
            status::RUNNING | status::CREATING => Status::Busy,
            status::ERROR => Status::Error,
            // Finished splits the same way Claude's list does: seen or not.
            status::FINISHED => {
                if self.is_unread.unwrap_or(false) {
                    Status::Unread
                } else {
                    Status::Idle
                }
            }
            status::EXPIRED => Status::Idle,
            _ => Status::Unknown,
        }
    }

    fn since(&self) -> u64 {
        self.last_message_activity_at_ms
            .or(self.updated_at)
            .unwrap_or(0)
    }

    /// Archived and killed agents are history, not status.
    fn listed(&self) -> bool {
        !self.is_archived.unwrap_or(false) && !self.is_killed.unwrap_or(false)
    }
}

fn store_path() -> Option<PathBuf> {
    let base = std::env::var("APPDATA")
        .ok()
        .filter(|d| !d.trim().is_empty())?;
    let path = PathBuf::from(base)
        .join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb");
    path.exists().then_some(path)
}

/// Reads every cloud-agent record out of the store.
fn read_agents(path: &Path) -> Vec<Agent> {
    // Read-only, and via a URI so SQLite does not try to create or recover anything: Cursor has
    // the same file open, and this program has no business writing to it.
    let uri = format!("file:{}?mode=ro", path.to_string_lossy().replace('\\', "/"));
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return Vec::new();
    };

    let Ok(mut statement) = connection
        .prepare("select value from ItemTable where key like 'cloudAgentRepository.agents%'")
    else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(0)) else {
        return Vec::new();
    };

    let mut agents = Vec::new();
    for value in rows.flatten() {
        // One key per account, each holding an array.
        if let Ok(parsed) = serde_json::from_str::<Vec<Agent>>(&value) {
            agents.extend(parsed);
        }
    }
    agents
}

/// A local chat, as `composerData:<id>` records it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Composer {
    name: Option<String>,
    status: Option<String>,
    last_updated_at: Option<u64>,
}

impl Composer {
    /// The chat's own title, when it has one worth showing.
    fn title(&self) -> Option<String> {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string)
    }

    /// `completed` and `aborted` are both endings; `none` is a chat that never ran. None of them
    /// is live — Cursor does not flush a running composer's state to disk.
    fn status(&self) -> Status {
        match self.status.as_deref() {
            Some("completed") | Some("aborted") => Status::Idle,
            _ => Status::Unknown,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Project {
    id: String,
    workspace: Option<Workspace>,
}

#[derive(Debug, Clone, Deserialize)]
struct Workspace {
    uri: Option<Uri>,
}

#[derive(Debug, Clone, Deserialize)]
struct Uri {
    #[serde(rename = "fsPath")]
    fs_path: Option<String>,
}

/// Folder leaf for a path, which is what a row shows as its name.
fn folder_leaf(path: &str) -> Option<String> {
    let leaf = path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next()?;
    (!leaf.trim().is_empty()).then(|| leaf.to_string())
}

/// One value out of the key/value table.
fn item(connection: &rusqlite::Connection, key: &str) -> Option<String> {
    connection
        .query_row("select value from ItemTable where key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .ok()
}

/// Maps every local chat to the folder it belongs to.
fn folders_by_conversation(connection: &rusqlite::Connection) -> HashMap<String, String> {
    let mut out = HashMap::new();

    let Some(projects) = item(connection, "glass.localAgentProjects.v1") else {
        return out;
    };
    let Ok(projects) = serde_json::from_str::<Vec<Project>>(&projects) else {
        return out;
    };
    let folders: HashMap<String, String> = projects
        .into_iter()
        .filter_map(|p| {
            let path = p.workspace?.uri?.fs_path?;
            Some((p.id, folder_leaf(&path)?))
        })
        .collect();

    let Some(membership) = item(connection, "glass.localAgentProjectMembership.v1") else {
        return out;
    };
    let Ok(membership) = serde_json::from_str::<HashMap<String, String>>(&membership) else {
        return out;
    };
    for (conversation, project) in membership {
        if let Some(folder) = folders.get(&project) {
            out.insert(conversation, folder.clone());
        }
    }
    out
}

/// Local chats touched within `days`.
///
/// The window is what keeps this honest: there are hundreds of these, none of them says anything
/// about a live state, and a list of every chat ever would bury the agents that do.
fn read_local(state: &rusqlite::Connection, days: u64, now_ms: u64) -> Vec<(String, u64, Composer)> {
    let Some(dir) = store_path().and_then(|p| p.parent().map(|d| d.to_path_buf())) else {
        return Vec::new();
    };
    let search = dir.join("conversation-search.db");
    let uri = format!(
        "file:{}?mode=ro",
        search.to_string_lossy().replace('\\', "/")
    );
    let Ok(index) = rusqlite::Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    ) else {
        return Vec::new();
    };

    let cutoff = now_ms.saturating_sub(days.saturating_mul(86_400_000));
    let Ok(mut statement) = index.prepare(
        "select id, updated_at from conversations where source = 'local' and is_archived = 0 \
         and updated_at >= ?1 order by updated_at desc limit 80",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = statement.query_map([cutoff as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
    }) else {
        return Vec::new();
    };

    // Only the handful inside the window are looked up, so the big conversation blobs are read
    // a few at a time rather than in their hundreds.
    rows.flatten()
        .filter_map(|(id, updated_at)| {
            let value = state
                .query_row(
                    "select value from cursorDiskKV where key = ?1",
                    [format!("composerData:{id}")],
                    |row| row.get::<_, String>(0),
                )
                .ok()?;
            let composer = serde_json::from_str::<Composer>(&value).ok()?;
            // A chat with no title is one Cursor itself shows nothing for — a scratch composer
            // that was opened and never used. It has nothing to say on a row.
            composer.title()?;
            Some((id, updated_at, composer))
        })
        .collect()
}

#[derive(Default)]
pub struct Cursor {
    scanned_at: u64,
    /// Window the cache was built for, so changing the setting takes effect at once.
    local_days: u64,
    cached: Vec<Session>,
    /// Cursor process with a window, so clicking a row can raise the app.
    window_pid: u32,
}

impl Cursor {
    pub fn new() -> Cursor {
        Cursor::default()
    }

    /// Cursor's sessions, rescanned at most every few seconds. `local_days` of 0 leaves local
    /// chats out and lists only cloud agents.
    pub fn sessions(&mut self, now_ms: u64, local_days: u64) -> Vec<Session> {
        if self.scanned_at != 0
            && self.local_days == local_days
            && now_ms.saturating_sub(self.scanned_at) < MIN_RESCAN_MS
        {
            return self.cached.clone();
        }
        self.scanned_at = now_ms;
        self.local_days = local_days;

        let Some(path) = store_path() else {
            self.cached = Vec::new();
            return Vec::new();
        };

        // Only worth resolving while there is something to click through to.
        self.window_pid = crate::activate::process_with_window("cursor.exe").unwrap_or(0);

        self.cached = read_agents(&path)
            .into_iter()
            .filter(Agent::listed)
            .map(|agent| Session {
                provider: Provider::Cursor,
                pid: self.window_pid,
                name: agent.repo(),
                cwd: agent.workspace_root_path.clone().unwrap_or_default(),
                title: agent
                    .name
                    .clone()
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty()),
                session_id: agent.bc_id.clone(),
                entrypoint: None,
                desktop_session_id: None,
                // Cursor's cloud-agent records carry no branch or pull request, so there is
                // nothing honest to mark these rows with.
                repo: crate::session::Repo::Nothing,
                status: agent.status(),
                waiting_for: None,
                since: agent.since(),
                // Cursor records no token usage on disk, so there is no cost to show.
                cost: None,
            })
            .collect();

        if local_days > 0 {
            self.cached.extend(self.local(&path, local_days, now_ms));
        }
        self.cached.clone()
    }

    /// Local chats as sessions. Their folder is their name, exactly as Cursor groups them.
    fn local(&self, path: &Path, days: u64, now_ms: u64) -> Vec<Session> {
        let uri = format!("file:{}?mode=ro", path.to_string_lossy().replace('\\', "/"));
        let Ok(state) = rusqlite::Connection::open_with_flags(
            &uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        ) else {
            return Vec::new();
        };

        let folders = folders_by_conversation(&state);
        read_local(&state, days, now_ms)
            .into_iter()
            .map(|(id, updated_at, composer)| Session {
                provider: Provider::Cursor,
                pid: self.window_pid,
                // Cursor groups these by folder, so the folder is what names the row.
                name: folders.get(&id).cloned().unwrap_or_else(|| "cursor".to_string()),
                cwd: String::new(),
                // The search index lags and often has no title for the newest chat; the composer
                // record has the real one.
                title: composer.title(),
                session_id: Some(id),
                entrypoint: None,
                desktop_session_id: None,
                // Cursor records no branch or pull request against a local chat.
                repo: crate::session::Repo::Nothing,
                status: composer.status(),
                waiting_for: None,
                // A composer record without a timestamp would date the row to the epoch, which
                // renders as "up 20697d"; the index knows when it was last touched.
                since: composer
                    .last_updated_at
                    .filter(|t| *t > 0)
                    .unwrap_or(updated_at),
                cost: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(status: i64, unread: bool) -> Agent {
        Agent {
            bc_id: Some("bc-1".to_string()),
            name: Some("Sfund-17664 snyk issues".to_string()),
            status: Some(status),
            is_unread: Some(unread),
            is_archived: Some(false),
            is_killed: Some(false),
            last_message_activity_at_ms: Some(1_000),
            updated_at: Some(500),
            repo_url: Some("https://github.com/nrccua/scale-fun-der.git".to_string()),
            workspace_root_path: Some(r"C:\git\scale-fun-der".to_string()),
        }
    }

    /// The mapping is only as good as the enum it came from: these are Cursor's own numbers.
    #[test]
    fn cursor_status_numbers_map_to_states() {
        assert_eq!(agent(status::RUNNING, false).status(), Status::Busy);
        assert_eq!(agent(status::CREATING, false).status(), Status::Busy);
        assert_eq!(agent(status::ERROR, false).status(), Status::Error);
        assert_eq!(agent(status::EXPIRED, false).status(), Status::Idle);
        assert_eq!(agent(0, false).status(), Status::Unknown);
    }

    /// Finished splits on the unread flag, exactly as the Claude list does.
    #[test]
    fn finished_splits_on_unread() {
        assert_eq!(agent(status::FINISHED, true).status(), Status::Unread);
        assert_eq!(agent(status::FINISHED, false).status(), Status::Idle);
    }

    #[test]
    fn archived_and_killed_agents_are_not_listed() {
        let mut a = agent(status::FINISHED, false);
        assert!(a.listed());
        a.is_archived = Some(true);
        assert!(!a.listed());
        a.is_archived = Some(false);
        a.is_killed = Some(true);
        assert!(!a.listed());
    }

    #[test]
    fn the_repository_name_comes_from_the_url_then_the_path() {
        assert_eq!(agent(1, false).repo(), "scale-fun-der");

        let mut a = agent(1, false);
        a.repo_url = None;
        assert_eq!(a.repo(), "scale-fun-der");

        a.workspace_root_path = None;
        assert_eq!(a.repo(), "cursor");
    }

    /// Last message activity is what the row's age should measure, not the record's update time.
    #[test]
    fn age_prefers_last_message_activity() {
        assert_eq!(agent(1, false).since(), 1_000);
        let mut a = agent(1, false);
        a.last_message_activity_at_ms = None;
        assert_eq!(a.since(), 500);
    }

    /// `completed` and `aborted` are endings; a local chat is never live.
    #[test]
    fn local_chat_status_is_only_ever_finished() {
        let ended = |s: &str| Composer {
            name: None,
            status: Some(s.to_string()),
            last_updated_at: None,
        }
        .status();
        assert_eq!(ended("completed"), Status::Idle);
        assert_eq!(ended("aborted"), Status::Idle);
        assert_eq!(ended("none"), Status::Unknown);
    }

    /// A scratch composer that was opened and never used has no title and nothing to say.
    #[test]
    fn untitled_chats_have_no_title() {
        let composer = |name: Option<&str>| Composer {
            name: name.map(str::to_string),
            status: None,
            last_updated_at: None,
        };
        assert_eq!(composer(Some("Cursor account balances")).title().as_deref(), Some("Cursor account balances"));
        assert!(composer(Some("   ")).title().is_none());
        assert!(composer(None).title().is_none());
    }

    #[test]
    fn folder_leaf_names_the_row() {
        assert_eq!(folder_leaf(r"c:\git\scale-fun-der").as_deref(), Some("scale-fun-der"));
        assert_eq!(folder_leaf("/home/x/repo/").as_deref(), Some("repo"));
        assert_eq!(folder_leaf(""), None);
    }

    /// Reads this machine's real Cursor store.
    /// `cargo test -- --nocapture live_cursor_agents`
    #[test]
    fn live_cursor_agents() {
        match store_path() {
            None => println!("no Cursor store on this machine"),
            Some(path) => {
                let agents = read_agents(&path);
                println!("{} cloud agents", agents.len());
                for a in agents.iter().take(10) {
                    println!(
                        "  {:?} status={:?} unread={:?} repo={} name={:?}",
                        a.status(),
                        a.status,
                        a.is_unread,
                        a.repo(),
                        a.name
                    );
                }
            }
        }
    }
}
