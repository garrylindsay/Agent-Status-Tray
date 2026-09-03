//! Conversation titles, read from Claude Code's own transcripts.
//!
//! The session registry only carries a derived name like `claude-tray-97`. The human title shown
//! in Claude's session list lives in the transcript at
//! `%USERPROFILE%\.claude\projects\<encoded cwd>\<sessionId>.jsonl`, as `custom-title` (set by
//! you) and `ai-title` (generated) records.
//!
//! Transcripts run to megabytes and grow constantly, so only the tail is read, and only when the
//! file has actually changed since it was last looked at.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use serde::Deserialize;

/// How much of the end of a transcript to search. Title records are rewritten as a conversation
/// goes on, so the most recent one is near the end.
const TAIL_BYTES: u64 = 256 * 1024;

/// A title record. Every other record type in the file parses to `None` for both fields.
#[derive(Deserialize)]
struct TitleLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "customTitle")]
    custom: Option<String>,
    #[serde(rename = "aiTitle")]
    ai: Option<String>,
}

/// `C:\git\claude-tray` becomes `C--git-claude-tray`, which is how Claude Code names the per-cwd
/// transcript directory. Windows matches the directory case-insensitively, so the casing of the
/// recorded cwd does not matter.
fn encode_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| match c {
            ':' | '\\' | '/' => '-',
            other => other,
        })
        .collect()
}

pub fn transcript_path(cwd: &str, session_id: &str) -> Option<PathBuf> {
    if cwd.trim().is_empty() || session_id.trim().is_empty() {
        return None;
    }
    let home = std::env::var("USERPROFILE")
        .ok()
        .filter(|h| !h.trim().is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".claude")
            .join("projects")
            .join(encode_cwd(cwd))
            .join(format!("{session_id}.jsonl")),
    )
}

/// Latest title in `text`, preferring one you set over one Claude generated.
fn title_from_tail(text: &str) -> Option<String> {
    let mut ai = None;

    // Latest first: a title set later supersedes an earlier one.
    for line in text.lines().rev() {
        if !line.contains("-title\"") {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<TitleLine>(line) else {
            continue;
        };
        match parsed.kind.as_str() {
            "custom-title" if let Some(title) = parsed.custom.filter(|t| !t.trim().is_empty()) => {
                return Some(title);
            }
            // Only the latest generated title is of interest, and lines are walked newest first.
            "ai-title" if ai.is_none() => {
                ai = parsed.ai.filter(|t| !t.trim().is_empty());
            }
            _ => {}
        }
    }
    ai
}

/// A title and the file size it was read at, so an unchanged transcript is not re-read.
struct Cached {
    len: u64,
    title: Option<String>,
}

#[derive(Default)]
pub struct Titles {
    cache: HashMap<String, Cached>,
}

impl Titles {
    pub fn new() -> Titles {
        Titles::default()
    }

    /// Title for a session, or `None` when it has no transcript or no title yet.
    pub fn get(&mut self, cwd: &str, session_id: &str) -> Option<String> {
        let path = transcript_path(cwd, session_id)?;
        let len = std::fs::metadata(&path).ok()?.len();

        if let Some(cached) = self.cache.get(session_id)
            && cached.len == len
        {
            return cached.title.clone();
        }

        let title = read_tail(&path, len).as_deref().and_then(title_from_tail);
        self.cache
            .insert(session_id.to_string(), Cached { len, title: title.clone() });
        title
    }

    /// Drops sessions that are no longer running, so the cache cannot grow without bound.
    pub fn retain(&mut self, live: &[String]) {
        self.cache.retain(|id, _| live.iter().any(|l| l == id));
    }
}

fn read_tail(path: &PathBuf, len: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let from = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(from)).ok()?;

    let mut buf = Vec::new();
    file.take(TAIL_BYTES + 1).read_to_end(&mut buf).ok()?;
    // A tail almost always starts mid-line, and lossy decoding can also split a character; the
    // partial first line is simply one fewer candidate.
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_encoding_matches_claude_codes_directory_names() {
        assert_eq!(encode_cwd(r"C:\git\claude-tray"), "C--git-claude-tray");
        assert_eq!(encode_cwd(r"C:\git\scale-fun-der"), "C--git-scale-fun-der");
        assert_eq!(encode_cwd("C:/git/TCC"), "C--git-TCC");
    }

    /// A title you set wins over a generated one, whichever order they appear in.
    #[test]
    fn a_custom_title_beats_a_generated_one() {
        let text = concat!(
            r#"{"type":"ai-title","aiTitle":"Claude-tray setup and run"}"#,
            "\n",
            r#"{"type":"custom-title","customTitle":"Claude-tray repo setup"}"#,
            "\n"
        );
        assert_eq!(title_from_tail(text).as_deref(), Some("Claude-tray repo setup"));

        // Also when the generated one was written last.
        let reversed = concat!(
            r#"{"type":"custom-title","customTitle":"Claude-tray repo setup"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Claude-tray setup and run"}"#,
            "\n"
        );
        assert_eq!(
            title_from_tail(reversed).as_deref(),
            Some("Claude-tray repo setup")
        );
    }

    #[test]
    fn the_latest_title_wins() {
        let text = concat!(
            r#"{"type":"custom-title","customTitle":"Old name"}"#,
            "\n",
            r#"{"type":"custom-title","customTitle":"New name"}"#,
            "\n"
        );
        assert_eq!(title_from_tail(text).as_deref(), Some("New name"));
    }

    #[test]
    fn a_generated_title_is_used_when_there_is_no_custom_one() {
        let text = r#"{"type":"ai-title","aiTitle":"Claude-tray setup and run"}"#;
        assert_eq!(
            title_from_tail(text).as_deref(),
            Some("Claude-tray setup and run")
        );
    }

    /// A tail starts mid-line and is full of other record types; neither may produce a title.
    #[test]
    fn partial_lines_and_other_records_are_ignored() {
        let text = concat!(
            r#"tent":"half a line that got cut","type":"assistant"}"#,
            "\n",
            r#"{"type":"user","content":"hello"}"#,
            "\n",
            r#"{"type":"custom-title","customTitle":""}"#,
            "\n"
        );
        assert_eq!(title_from_tail(text), None);
    }

    #[test]
    fn no_title_records_means_no_title() {
        assert_eq!(title_from_tail(r#"{"type":"user","content":"hi"}"#), None);
        assert_eq!(title_from_tail(""), None);
    }

    /// Reads whatever this machine's own sessions have.
    /// `cargo test -- --nocapture live_titles`
    #[test]
    fn live_titles() {
        let mut titles = Titles::new();
        for session in crate::session::Registry::new().scan() {
            let title = session
                .session_id
                .as_deref()
                .and_then(|id| titles.get(&session.cwd, id));
            println!("{} [{}] -> {:?}", session.name, session.cwd, title);
        }
    }
}
