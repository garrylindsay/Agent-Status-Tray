//! A small record of things that went wrong, for the settings panel to show.
//!
//! Everything else this program reads is a local file, where a failure means one row is missing and
//! there is nothing to say about it. The Cursor usage call is different: it goes over the network,
//! to an endpoint nobody documents, with a token that expires. When it stops working the cost
//! column simply empties, and without somewhere to look that is indistinguishable from "these
//! conversations cost nothing".
//!
//! So the rule is narrow: record what would otherwise be silent. Not a trace log.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// Enough to see a pattern -- a token that expired, a run of timeouts -- without ever being a file
/// on disk. Nothing here survives a restart, deliberately: it is a window into this run.
const KEEP: usize = 40;

#[derive(Clone)]
pub struct Entry {
    /// Epoch ms, so the panel can say how long ago without keeping a clock of its own.
    pub at_ms: u64,
    pub text: String,
}

fn store() -> &'static Mutex<VecDeque<Entry>> {
    static LOG: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// The whole of the recording rule, over a log passed in.
///
/// Kept separate from the global so it can be tested against a log of its own: the process has one
/// log, tests run in parallel, and a test that clears a shared one is really testing the scheduler.
fn push_into(log: &mut VecDeque<Entry>, text: String, now_ms: u64) {
    // A poll that fails once usually fails every time until something changes, and forty copies of
    // one timeout would push out the message that explains it. A repeat keeps its place and takes
    // the newer time, so the log shows when it last happened rather than a wall of duplicates.
    if let Some(last) = log.front_mut()
        && last.text == text
    {
        last.at_ms = now_ms;
        return;
    }

    log.push_front(Entry { at_ms: now_ms, text });
    while log.len() > KEEP {
        log.pop_back();
    }
}

/// Records a failure, unless it repeats the message already at the top.
pub fn record(text: impl Into<String>, now_ms: u64) {
    if let Ok(mut log) = store().lock() {
        push_into(&mut log, text.into(), now_ms);
    }
}

/// Newest first.
pub fn entries() -> Vec<Entry> {
    store()
        .lock()
        .map(|log| log.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn is_empty() -> bool {
    store().lock().map(|log| log.is_empty()).unwrap_or(true)
}

/// Writes the log out and hands it to whatever opens a text file.
///
/// The settings panel already fills a modest screen, and a log wants width, wrapping and scrolling
/// that a column of 24px rows cannot give it. A text file gets all three for free, and can be
/// copied into a bug report. It goes to the temp directory because it is a snapshot of this run,
/// not a record worth keeping.
pub fn open() {
    let path = std::env::temp_dir().join("agent-status-tray-log.txt");

    let mut text = String::from("agent-status-tray log
Newest first. This run only.

");
    let now = crate::now_ms();
    for entry in entries() {
        text.push_str(&format!(
            "{:>8} ago  {}
",
            crate::render::elapsed(now.saturating_sub(entry.at_ms)),
            entry.text
        ));
    }

    if std::fs::write(&path, text).is_err() {
        return;
    }
    crate::activate::open_path(&path);
}

/// Forgets everything, for the panel's clear button.
pub fn clear() {
    if let Ok(mut log) = store().lock() {
        log.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_of(entries: &[(&str, u64)]) -> VecDeque<Entry> {
        let mut log = VecDeque::new();
        for (text, at) in entries {
            push_into(&mut log, text.to_string(), *at);
        }
        log
    }

    #[test]
    fn the_newest_entry_comes_first() {
        let log = log_of(&[("first", 1_000), ("second", 2_000)]);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].text, "second");
        assert_eq!(log[1].text, "first");
    }

    /// A failure that repeats every poll must not bury the entry that explains it.
    #[test]
    fn a_repeated_message_is_touched_rather_than_stacked() {
        let mut log = log_of(&[("something explanatory", 1_000)]);
        for tick in 0..100 {
            push_into(&mut log, "the same timeout".to_string(), 5_000 + tick);
        }
        assert_eq!(log.len(), 2, "the repeat should occupy one slot");
        assert_eq!(log[0].text, "the same timeout");
        assert_eq!(log[0].at_ms, 5_099, "carrying the latest time");
        assert_eq!(log[1].text, "something explanatory", "and not pushing out the cause");
    }

    /// Two failures alternating are each their own entry: only an immediate repeat collapses.
    #[test]
    fn alternating_failures_are_both_kept() {
        let log = log_of(&[("timed out", 1), ("rejected", 2), ("timed out", 3)]);
        assert_eq!(log.len(), 3);
    }

    #[test]
    fn the_log_stays_bounded() {
        let mut log = VecDeque::new();
        for i in 0..KEEP * 3 {
            push_into(&mut log, format!("failure {i}"), i as u64);
        }
        assert_eq!(log.len(), KEEP);
        assert_eq!(log[0].text, format!("failure {}", KEEP * 3 - 1), "newest kept");
    }
}
