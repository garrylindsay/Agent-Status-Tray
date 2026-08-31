//! Text for the menu rows and the hover tooltip.

use crate::session::{Session, Status};

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

/// One menu row: `● api-gateway-f6 — WAITING 4m · permission prompt`
pub fn row(session: &Session, now_ms: u64) -> String {
    let age = now_ms.saturating_sub(session.since);
    let mut text = format!(
        "{} {} \u{2014} {} {}",
        session.status.glyph(),
        session.name,
        session.status.label(),
        elapsed(age)
    );
    if let Some(reason) = &session.waiting_for {
        text.push_str(" \u{00b7} ");
        text.push_str(reason);
    }
    escape_mnemonics(&text)
}

pub fn header(sessions: &[Session]) -> String {
    match sessions.len() {
        0 => "No Claude sessions".to_string(),
        1 => "1 Claude session".to_string(),
        n => format!("{n} Claude sessions"),
    }
}

/// Hover text: `4 sessions · 1 waiting · 1 busy`
pub fn tooltip(sessions: &[Session]) -> String {
    if sessions.is_empty() {
        return "No Claude sessions".to_string();
    }

    let waiting = sessions
        .iter()
        .filter(|s| s.status == Status::Waiting)
        .count();
    let busy = sessions
        .iter()
        .filter(|s| matches!(s.status, Status::Busy | Status::Shell))
        .count();
    let idle = sessions.iter().filter(|s| s.status == Status::Idle).count();

    let mut parts = vec![format!(
        "{} session{}",
        sessions.len(),
        if sessions.len() == 1 { "" } else { "s" }
    )];
    if waiting > 0 {
        parts.push(format!("{waiting} waiting"));
    }
    if busy > 0 {
        parts.push(format!("{busy} busy"));
    }
    if idle > 0 {
        parts.push(format!("{idle} idle"));
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

    #[test]
    fn ampersands_are_escaped_for_menus() {
        let session = Session {
            pid: 1,
            key: "r&d-tool".to_string(),
            name: "r&d-tool".to_string(),
            status: Status::Idle,
            waiting_for: None,
            since: 0,
        };
        assert!(row(&session, 0).contains("r&&d-tool"));
    }
}
