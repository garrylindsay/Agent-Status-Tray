//! Gives every session a color of its own, so a menu row is recognized rather than read.
//!
//! The color says *which* session; the glyph and label on the row still say what it is doing.

use std::collections::HashMap;

use crate::icon::Rgba;
use crate::session::Session;

/// Mid-saturation hues that stay legible on both the light and the dark menu background,
/// and that steer clear of the icon's status red and status blue so a chip is never
/// mistaken for a status.
pub const PALETTE: [Rgba; 12] = [
    [0x4C, 0x8D, 0xD9, 0xFF], // blue
    [0x3F, 0xA7, 0x96, 0xFF], // teal
    [0x4F, 0xA3, 0x52, 0xFF], // green
    [0x8D, 0xA8, 0x2E, 0xFF], // olive
    [0xD1, 0xA3, 0x2B, 0xFF], // gold
    [0xE0, 0x80, 0x3A, 0xFF], // orange
    [0xDB, 0x5F, 0x52, 0xFF], // coral
    [0xD4, 0x56, 0x8C, 0xFF], // pink
    [0xB0, 0x64, 0xCF, 0xFF], // violet
    [0x7B, 0x6F, 0xE0, 0xFF], // indigo
    [0x5F, 0x97, 0xB8, 0xFF], // steel
    [0x9B, 0x7B, 0x5C, 0xFF], // clay
];

/// FNV-1a, so a key lands on the same slot in every process without pulling in a hasher.
fn hash_index(key: &str) -> usize {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % PALETTE.len() as u64) as usize
}

/// Sticky palette assignment.
///
/// Hashing alone collides too often to be useful at these sizes — four sessions across
/// twelve slots duplicate about a third of the time — so a key that finds its slot taken
/// probes forward to a free one. The assignment then sticks until the session disappears,
/// which keeps a row's color still while its status and position change around it.
#[derive(Default)]
pub struct ColorMap {
    assigned: HashMap<String, usize>,
}

impl ColorMap {
    pub fn new() -> ColorMap {
        ColorMap::default()
    }

    pub fn index_for(&mut self, key: &str) -> usize {
        if let Some(&index) = self.assigned.get(key) {
            return index;
        }

        let start = hash_index(key);
        // Falls back to the hashed slot once every color is spoken for.
        let index = (0..PALETTE.len())
            .map(|step| (start + step) % PALETTE.len())
            .find(|slot| !self.assigned.values().any(|taken| taken == slot))
            .unwrap_or(start);

        self.assigned.insert(key.to_string(), index);
        index
    }

    /// Release the slots of sessions that are gone. Call before assigning for a tick.
    pub fn retain_live(&mut self, sessions: &[Session]) {
        self.assigned
            .retain(|key, _| sessions.iter().any(|session| &session.key == key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Status;

    fn session(key: &str) -> Session {
        Session {
            pid: 1,
            key: key.to_string(),
            name: key.to_string(),
            status: Status::Idle,
            waiting_for: None,
            since: 0,
        }
    }

    #[test]
    fn a_key_keeps_its_color() {
        let mut colors = ColorMap::new();
        let first = colors.index_for("session-a");
        assert_eq!(colors.index_for("session-a"), first);
        colors.index_for("session-b");
        assert_eq!(colors.index_for("session-a"), first);
    }

    #[test]
    fn hashing_is_stable_across_instances() {
        assert_eq!(hash_index("session-a"), hash_index("session-a"));
        assert!(hash_index("session-a") < PALETTE.len());
    }

    /// The whole point of the probe: two live sessions must never share a chip.
    #[test]
    fn live_sessions_never_share_a_slot() {
        let mut colors = ColorMap::new();
        let keys: Vec<String> = (0..PALETTE.len()).map(|i| format!("s{i}")).collect();
        let mut seen = Vec::new();
        for key in &keys {
            let index = colors.index_for(key);
            assert!(!seen.contains(&index), "slot {index} handed out twice");
            seen.push(index);
        }
    }

    #[test]
    fn a_departed_session_frees_its_slot() {
        let mut colors = ColorMap::new();
        let first = colors.index_for("ghost");
        colors.retain_live(&[]);
        // With every slot free again, a fresh key can land on the one "ghost" held.
        let mut reused = ColorMap::new();
        assert_eq!(reused.index_for("ghost"), first);
        assert!(colors.assigned.is_empty());
    }

    #[test]
    fn retain_live_keeps_running_sessions() {
        let mut colors = ColorMap::new();
        colors.index_for("alive");
        colors.index_for("dead");
        colors.retain_live(&[session("alive")]);
        assert!(colors.assigned.contains_key("alive"));
        assert!(!colors.assigned.contains_key("dead"));
    }
}
