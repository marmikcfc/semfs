//! Per-daemon session memory for cross-turn `grep` dedup (Linear SEM-19, v1).
//!
//! Each `semfs grep` invocation is a fresh process, so it cannot remember what
//! earlier searches returned. The long-lived daemon can. This is that memory.
//!
//! v1 assumption: **one mount = one agent**, so a single daemon-global sliding
//! window IS the session — no session id / keying (see v2 / `SO_PEERCRED` in the
//! ticket). We track the files returned WITH CONTENT in the last `window`
//! searches; on a later search that re-surfaces such a file, the daemon marks it
//! "seen" and strips its content so the agent isn't re-charged for bytes already
//! in its context. The principle is DIFF, never REPLAY.
//!
//! Failure shape is intentionally soft: an over-suppression only ever means the
//! agent must re-read a file it was pointed to (one extra read), never lost data.

use std::collections::HashMap;

/// A bounded, recency-windowed record of which files were already returned with
/// content this session. Cheap: one `HashMap<path, turn>` + a counter.
#[derive(Debug)]
pub struct SessionCache {
    /// Sliding-window size W: a file sent at turn T is deduped while
    /// `current_turn - T < window`. Constructed only when W > 0 (0 = disabled,
    /// handled by the caller storing `None`).
    window: u64,
    /// Monotonic counter — the Nth search this session. Doubles as the "turn N"
    /// shown to the agent in the pointer line.
    turn: u64,
    /// filepath -> turn it was first sent with content (within the window).
    seen: HashMap<String, u64>,
}

impl SessionCache {
    /// `window` = how many recent searches to remember (W). Caller passes > 0.
    pub fn new(window: u64) -> Self {
        Self { window, turn: 0, seen: HashMap::new() }
    }

    /// Begin a new search turn: advance the counter and evict entries that have
    /// fallen outside the sliding window. Returns the new turn number.
    pub fn begin_turn(&mut self) -> u64 {
        self.turn += 1;
        let cutoff = self.turn.saturating_sub(self.window);
        self.seen.retain(|_, &mut t| t > cutoff);
        self.turn
    }

    /// Record or look up a content-bearing hit at the current turn.
    /// - already seen within the window → `Some(first_turn)` (do NOT resend)
    /// - first time → record it at the current turn, return `None` (send content)
    pub fn see(&mut self, filepath: &str) -> Option<u64> {
        if let Some(&first_turn) = self.seen.get(filepath) {
            Some(first_turn)
        } else {
            self.seen.insert(filepath.to_string(), self.turn);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_turn_is_one_then_increments() {
        let mut c = SessionCache::new(5);
        assert_eq!(c.begin_turn(), 1);
        assert_eq!(c.begin_turn(), 2);
        assert_eq!(c.begin_turn(), 3);
    }

    #[test]
    fn first_sighting_returns_none_and_records() {
        let mut c = SessionCache::new(5);
        c.begin_turn(); // turn 1
        assert_eq!(c.see("/a.md"), None, "first sighting must send content");
    }

    #[test]
    fn resighting_within_window_returns_first_turn() {
        let mut c = SessionCache::new(5);
        c.begin_turn(); // turn 1
        assert_eq!(c.see("/a.md"), None); // first send, recorded @1
        c.begin_turn(); // turn 2
        assert_eq!(c.see("/a.md"), Some(1), "re-grep must report the original turn, not resend");
    }

    #[test]
    fn distinct_files_are_independent() {
        let mut c = SessionCache::new(5);
        c.begin_turn();
        assert_eq!(c.see("/a.md"), None);
        assert_eq!(c.see("/b.md"), None, "a different file is still new");
        c.begin_turn();
        assert_eq!(c.see("/a.md"), Some(1));
        assert_eq!(c.see("/b.md"), Some(1));
    }

    #[test]
    fn eviction_after_window_allows_resend() {
        // W=2: file seen at turn 1 is remembered on turns 1 and 2, forgotten at turn 3.
        let mut c = SessionCache::new(2);
        c.begin_turn(); // 1
        assert_eq!(c.see("/a.md"), None); // recorded @1
        c.begin_turn(); // 2
        assert_eq!(c.see("/a.md"), Some(1), "still within window at turn 2");
        c.begin_turn(); // 3 — turn 1 now outside W=2 window, evicted
        assert_eq!(c.see("/a.md"), None, "aged out → safe to resend (content may have left context)");
    }

    #[test]
    fn resighting_does_not_refresh_the_recorded_turn() {
        // Re-seeing a file reports its ORIGINAL turn and does not slide it forward,
        // so the window is anchored to first delivery (predictable eviction).
        let mut c = SessionCache::new(5);
        c.begin_turn(); // 1
        c.see("/a.md"); // @1
        c.begin_turn(); // 2
        assert_eq!(c.see("/a.md"), Some(1));
        c.begin_turn(); // 3
        assert_eq!(c.see("/a.md"), Some(1), "still anchored to turn 1");
    }
}
