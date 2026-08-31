//! Decides *when* an alert fires. Kept free of Win32 so the rules can be tested directly.

use crate::config::Config;

/// Tracks what was alerted on last, so a steady state repeats on schedule while a newly-blocked
/// session interrupts immediately.
#[derive(Debug, Default)]
pub struct Alerter {
    last_fired_ms: Option<u64>,
    /// Pids that were matching at the last fire.
    known: Vec<u32>,
}

impl Alerter {
    pub fn new() -> Alerter {
        Alerter::default()
    }

    /// `matching` is the pids currently in a watched status. Returns true when an alert is due.
    ///
    /// Two things trigger one: a pid that was not in the previous alert (so a session that just
    /// blocked tells you now, rather than up to a repeat-interval later), or the repeat interval
    /// elapsing while at least one session is still waiting on you.
    pub fn should_fire(&mut self, matching: &[u32], now_ms: u64, config: &Config) -> bool {
        if !config.notifications_enabled || config.notify_statuses.is_empty() {
            return false;
        }

        if matching.is_empty() {
            // Nothing is waiting: forget the schedule so the next one alerts immediately.
            self.reset();
            return false;
        }

        let has_new = matching.iter().any(|pid| !self.known.contains(pid));
        let due = match (self.last_fired_ms, config.repeat_secs) {
            (None, _) => true,
            (Some(_), 0) => false, // "Only once" — until the set changes.
            (Some(last), repeat) => now_ms.saturating_sub(last) >= repeat * 1_000,
        };

        if has_new || due {
            self.last_fired_ms = Some(now_ms);
            self.known = matching.to_vec();
            return true;
        }
        false
    }

    /// Called when the alert condition clears, and after a manual test alert so the test does not
    /// shift the repeat schedule.
    pub fn reset(&mut self) {
        self.last_fired_ms = None;
        self.known.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Status;

    fn config() -> Config {
        Config::default()
    }

    #[test]
    fn first_match_fires_immediately() {
        let mut a = Alerter::new();
        assert!(a.should_fire(&[100], 0, &config()));
    }

    #[test]
    fn steady_state_repeats_on_the_configured_interval() {
        let mut a = Alerter::new();
        let c = config(); // 60s
        assert!(a.should_fire(&[100], 0, &c));
        assert!(!a.should_fire(&[100], 30_000, &c));
        assert!(!a.should_fire(&[100], 59_000, &c));
        assert!(a.should_fire(&[100], 60_000, &c));
        assert!(!a.should_fire(&[100], 61_000, &c));
        assert!(a.should_fire(&[100], 120_000, &c));
    }

    #[test]
    fn a_newly_blocked_session_interrupts_the_schedule() {
        let mut a = Alerter::new();
        let c = config();
        assert!(a.should_fire(&[100], 0, &c));
        // A second session blocks 5s later: worth saying so now.
        assert!(a.should_fire(&[100, 200], 5_000, &c));
        assert!(!a.should_fire(&[100, 200], 6_000, &c));
    }

    #[test]
    fn clearing_the_condition_rearms_immediately() {
        let mut a = Alerter::new();
        let c = config();
        assert!(a.should_fire(&[100], 0, &c));
        assert!(!a.should_fire(&[], 1_000, &c));
        // Same pid blocks again a second later — not throttled by the earlier fire.
        assert!(a.should_fire(&[100], 2_000, &c));
    }

    #[test]
    fn only_once_never_repeats_for_an_unchanged_set() {
        let mut a = Alerter::new();
        let c = Config {
            repeat_secs: 0,
            ..config()
        };
        assert!(a.should_fire(&[100], 0, &c));
        assert!(!a.should_fire(&[100], 3_600_000, &c));
        // But a new session still speaks up.
        assert!(a.should_fire(&[100, 200], 3_600_001, &c));
    }

    #[test]
    fn disabled_notifications_never_fire() {
        let mut a = Alerter::new();
        let c = Config {
            notifications_enabled: false,
            ..config()
        };
        assert!(!a.should_fire(&[100], 0, &c));
    }

    #[test]
    fn an_empty_status_selection_never_fires() {
        let mut a = Alerter::new();
        let c = Config {
            notify_statuses: vec![],
            ..config()
        };
        assert!(!a.should_fire(&[100], 0, &c));
    }

    #[test]
    fn watching_extra_statuses_is_honoured_by_the_caller() {
        // `should_fire` takes an already-filtered list; this documents the contract.
        let mut c = config();
        c.toggle_status(Status::Idle);
        assert!(c.notifies_on(Status::Idle));
        assert!(c.notifies_on(Status::Waiting));
    }
}
