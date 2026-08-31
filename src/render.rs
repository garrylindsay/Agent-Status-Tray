//! Text for the menu rows and the hover tooltip.

use crate::session::{Repo, Session, Status};

/// Windows caps `NOTIFYICONDATA.szTip` at 128 wide chars.
const TOOLTIP_MAX: usize = 120;

/// Compact duration: `45s`, `1m12s`, `4m`, `2h3m`.
pub fn elapsed(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=599 => {
            let (m, s) = (secs / 60, secs % 60);
            if s == 0 {
                format!("{m}m")
            } else {
                format!("{m}m{s}s")
            }
        }
        600..=3599 => format!("{}m", secs / 60),
        // Past a couple of days, hours stop being readable: 8629 minutes means nothing.
        172_800.. => format!("{}d", secs / 86_400),
        _ => {
            let (h, m) = (secs / 3600, (secs % 3600) / 60);
            if m == 0 {
                format!("{h}h")
            } else {
                format!("{h}h{m}m")
            }
        }
    }
}

/// Windows menus read a lone `&` as a mnemonic marker.
fn escape_mnemonics(text: &str) -> String {
    text.replace('&', "&&")
}

/// One row: `api-gateway-f6 — WAITING 4m · permission prompt`
///
/// The status dot is drawn rather than written, so it is not part of this text.
///
/// A session whose status was never reported gets `◌ name — up 4h36m` instead. Printing a `?`
/// where a status belongs reads as a state the session is in, and the duration beside it is the
/// session's age rather than the age of any status, so it is labelled as uptime and nothing is
/// claimed about what the session is doing.
/// Words for a repository state, for surfaces that cannot draw the mark.
fn repo_text(repo: Repo) -> Option<&'static str> {
    match repo {
        Repo::Nothing => None,
        Repo::Branch => Some("branch"),
        Repo::PrOpen => Some("PR open"),
        Repo::PrMerged => Some("PR merged"),
    }
}

fn row_text(session: &Session, now_ms: u64) -> String {
    let age = elapsed(now_ms.saturating_sub(session.since));
    // Rows from different tools sit in one list, so each says which tool it came from.
    let name = match &session.title {
        // The name says where a session is but not what it is about; the conversation's own title
        // is what tells two sessions in the same place apart.
        Some(title) if title != &session.name => format!(
            "{}/{} \u{00b7} {}",
            session.provider.label(),
            session.name,
            title
        ),
        _ => format!("{}/{}", session.provider.label(), session.name),
    };

    if session.status == Status::Unknown {
        return format!("{name} \u{2014} up {age}");
    }

    let mut text = format!("{name} \u{2014} {} {}", session.status.label(), age);
    if let Some(reason) = &session.waiting_for {
        text.push_str(" \u{00b7} ");
        text.push_str(reason);
    }
    text
}

/// Row for the tray menu, with `&` escaped for the menu's mnemonic parser.
///
/// A menu item can carry one icon and that is the status dot, so the repository state has to be
/// said in words here rather than drawn.
pub fn row(session: &Session, now_ms: u64) -> String {
    let mut text = row_text(session, now_ms);
    if let Some(repo) = repo_text(session.repo) {
        text.push_str(" \u{00b7} ");
        text.push_str(repo);
    }
    escape_mnemonics(&text)
}

/// Row for the popup, which draws with `DT_NOPREFIX` and so wants the name verbatim.
///
/// No repository words here: the popup draws the mark instead, and saying it twice is noise.
pub fn alert_row(session: &Session, now_ms: u64) -> String {
    row_text(session, now_ms)
}

/// Popup heading: names what is actually wanted from you, and only when that is actually known.
///
/// Sessions whose status was never reported must not be described as needing attention — nothing
/// about them says so, and an alert that overstates what it knows is worse than a vague one.
pub fn alert_title(sessions: &[Session]) -> String {
    let n = sessions.len();
    if n == 0 {
        return "No agent sessions".to_string();
    }

    if sessions.iter().any(|s| s.status == Status::Error) {
        let failed = sessions.iter().filter(|s| s.status == Status::Error).count();
        return if failed == 1 {
            "1 agent failed".to_string()
        } else {
            format!("{failed} agents failed")
        };
    }

    if sessions.iter().all(|s| s.status == Status::Waiting) {
        return if n == 1 {
            "1 session is waiting on you".to_string()
        } else {
            format!("{n} sessions are waiting on you")
        };
    }

    if sessions.iter().all(|s| s.status == Status::Unread) {
        return if n == 1 {
            "1 session finished".to_string()
        } else {
            format!("{n} sessions finished")
        };
    }

    if sessions.iter().all(|s| s.status == Status::Unknown) {
        return if n == 1 {
            "1 session open".to_string()
        } else {
            format!("{n} sessions open")
        };
    }

    if n == 1 {
        "1 session needs attention".to_string()
    } else {
        format!("{n} sessions need attention")
    }
}

pub fn header(sessions: &[Session]) -> String {
    match sessions.len() {
        0 => "No agent sessions".to_string(),
        1 => "1 agent session".to_string(),
        n => format!("{n} agent sessions"),
    }
}

/// Hover text: `4 sessions · 1 waiting · 1 busy`
pub fn tooltip(sessions: &[Session]) -> String {
    if sessions.is_empty() {
        return "No agent sessions".to_string();
    }

    let waiting = sessions
        .iter()
        .filter(|s| s.status == Status::Waiting)
        .count();
    let busy = sessions
        .iter()
        .filter(|s| matches!(s.status, Status::Busy | Status::Shell))
        .count();
    let failed = sessions.iter().filter(|s| s.status == Status::Error).count();
    let unread = sessions.iter().filter(|s| s.status == Status::Unread).count();
    let idle = sessions.iter().filter(|s| s.status == Status::Idle).count();
    let unknown = sessions
        .iter()
        .filter(|s| s.status == Status::Unknown)
        .count();

    let mut parts = vec![format!(
        "{} session{}",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" }
    )];
    if waiting > 0 {
        parts.push(format!("{waiting} waiting"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if busy > 0 {
        parts.push(format!("{busy} busy"));
    }
    if unread > 0 {
        parts.push(format!("{unread} unread"));
    }
    if idle > 0 {
        parts.push(format!("{idle} idle"));
    }
    if unknown == sessions.len() {
        parts.push("state not reported".to_string());
    } else if unknown > 0 {
        parts.push(format!("{unknown} not reported"));
    }

    let mut text = parts.join(" \u{00b7} ");
    if text.chars().count() > TOOLTIP_MAX {
        text = text.chars().take(TOOLTIP_MAX).collect();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_formats() {
        assert_eq!(elapsed(0), "0s");
        assert_eq!(elapsed(45_000), "45s");
        assert_eq!(elapsed(72_000), "1m12s");
        assert_eq!(elapsed(120_000), "2m");
        assert_eq!(elapsed(900_000), "15m");
        assert_eq!(elapsed(7_380_000), "2h3m");
        assert_eq!(elapsed(7_200_000), "2h");
    }

    fn session(name: &str, status: Status) -> Session {
        Session {
            provider: crate::session::Provider::ClaudeCode,
            pid: 1,
            name: name.to_string(),
            cwd: String::new(),
            title: None,
            session_id: None,
            entrypoint: None,
            desktop_session_id: None,
            repo: Repo::Nothing,
            status,
            waiting_for: None,
            since: 0,
        }
    }

    /// The menu says the repository state in words; the popup draws it and must not repeat it.
    #[test]
    fn only_the_menu_spells_out_the_repository_state() {
        let mut s = session("claude-tray-97", Status::Idle);
        s.repo = Repo::PrOpen;
        assert!(row(&s, 0).contains("PR open"), "menu row: {}", row(&s, 0));
        assert!(!alert_row(&s, 0).contains("PR open"));

        s.repo = Repo::Nothing;
        assert!(!row(&s, 0).contains("PR"));
    }

    #[test]
    fn ampersands_are_escaped_for_menus() {
        assert!(row(&session("r&d-tool", Status::Idle), 0).contains("r&&d-tool"));
    }

    /// A `?` where a status belongs reads as a state the session is in, and the duration beside
    /// it is the session's age, not a status age.
    #[test]
    fn an_unreported_status_is_shown_as_uptime() {
        let row = row_text(&session("tcc-35", Status::Unknown), 16_360_000);
        assert!(row.contains("up 4h32m"), "got {row}");
        assert!(!row.contains('?'), "still claims a status: {row}");
    }

    #[test]
    fn a_known_status_still_reads_as_before() {
        let mut s = session("api-gateway-f6", Status::Waiting);
        s.waiting_for = Some("permission prompt".to_string());
        let row = row_text(&s, 240_000);
        assert!(row.contains("WAITING 4m \u{b7} permission prompt"), "got {row}");
    }

    /// Nothing about an unreported session says it needs anything.
    #[test]
    fn unreported_sessions_are_not_called_attention_worthy() {
        let sessions = vec![
            session("a", Status::Unknown),
            session("b", Status::Unknown),
        ];
        assert_eq!(alert_title(&sessions), "2 sessions open");
        assert!(tooltip(&sessions).contains("state not reported"));
    }

    #[test]
    fn waiting_sessions_still_say_so() {
        let sessions = vec![session("a", Status::Waiting)];
        assert_eq!(alert_title(&sessions), "1 session is waiting on you");
    }

    /// A mix is only as strong as its weakest claim, but something really is waiting.
    #[test]
    fn a_mixed_set_falls_back_to_attention() {
        let sessions = vec![
            session("a", Status::Waiting),
            session("b", Status::Unknown),
        ];
        assert_eq!(alert_title(&sessions), "2 sessions need attention");
        assert!(tooltip(&sessions).contains("1 not reported"));
    }
}
