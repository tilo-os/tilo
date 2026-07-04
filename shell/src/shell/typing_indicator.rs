//! Transient per-peer action indicators (typing, moving, resizing).
//!
//! Peers announce activity via `PresencePayload::CurrentAction`. Rapid
//! streams of keystrokes would otherwise flood the render pass with
//! churn; this module debounces incoming notes so we produce at most one
//! sustained indicator per peer, and clear it after [`SILENCE_TIMEOUT`]
//! of no new notes.
//!
//! The tracker is time-source-injectable so tests can drive it in
//! virtual time without sleeping.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// After this long without a fresh note, the indicator disappears.
pub const SILENCE_TIMEOUT: Duration = Duration::from_millis(200);

/// A rendered indicator for a peer's current action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionIndicator {
    pub user_id: String,
    pub action: String,
}

/// Per-peer debounce state.
#[derive(Clone, Copy, Debug)]
struct Entry {
    last_note: Instant,
    /// Which action the peer most recently announced. Kept keyed by
    /// user rather than by (user, action) so a peer transitioning from
    /// "typing" to "resizing" replaces the visible indicator rather
    /// than showing both.
    action_slot: usize,
}

/// State machine over presence-hint notes. Not thread-safe; owned by the
/// shell's single-threaded render pass.
#[derive(Default)]
pub struct TypingTracker {
    /// (peer, action) → debounce entry. Multiple actions per peer are
    /// tracked separately so switching from "typing" to "resizing"
    /// immediately swaps which indicator is active.
    entries: HashMap<(String, String), Entry>,
    /// Monotonic counter to break ties for "which action shows for a
    /// peer with multiple active notes": the highest slot wins.
    next_slot: usize,
}

impl TypingTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a note from a peer. Rapid repeats within `SILENCE_TIMEOUT`
    /// are merged — the indicator stays lit, just with its expiry
    /// pushed out. Different actions for the same peer replace each
    /// other on the visible slot.
    pub fn note(&mut self, user_id: &str, action: &str, t: Instant) {
        let slot = self.next_slot;
        self.next_slot = self.next_slot.wrapping_add(1);
        self.entries.insert(
            (user_id.to_string(), action.to_string()),
            Entry {
                last_note: t,
                action_slot: slot,
            },
        );
    }

    /// The set of indicators that should be visible at instant `t`. At
    /// most one indicator per peer — the most-recently-noted action wins.
    pub fn active_at(&self, t: Instant) -> Vec<ActionIndicator> {
        let mut latest_per_peer: HashMap<&str, (&str, Entry)> = HashMap::new();
        for ((uid, action), entry) in &self.entries {
            let age = t.saturating_duration_since(entry.last_note);
            if age > SILENCE_TIMEOUT {
                continue;
            }
            // Newer note wins per peer.
            match latest_per_peer.get(uid.as_str()) {
                Some((_, existing)) if existing.action_slot >= entry.action_slot => {}
                _ => {
                    latest_per_peer.insert(uid.as_str(), (action.as_str(), *entry));
                }
            }
        }
        let mut out: Vec<ActionIndicator> = latest_per_peer
            .into_iter()
            .map(|(uid, (action, _))| ActionIndicator {
                user_id: uid.to_string(),
                action: action.to_string(),
            })
            .collect();
        // Deterministic order for tests.
        out.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        out
    }

    /// Drop expired entries. Callers can invoke this periodically to keep
    /// the map small under long sessions.
    pub fn gc(&mut self, t: Instant) {
        self.entries
            .retain(|_, e| t.saturating_duration_since(e.last_note) <= SILENCE_TIMEOUT);
    }

    /// Explicitly clear a peer (e.g. on `Left`).
    pub fn clear_peer(&mut self, user_id: &str) {
        self.entries.retain(|(u, _), _| u != user_id);
    }
}

/// Client-side debouncer for the emit path.
///
/// Widgets that generate typing events call [`Self::should_send`] on
/// every keystroke. It returns `true` only when either:
///   - This is the first keystroke in a new burst, or
///   - The [`RESEND_INTERVAL`] has elapsed since the last emitted event
///     and there's still activity.
///
/// This means one emit at the start of a burst plus one refresh every
/// ~150ms while typing continues, rather than one per keystroke. Well
/// under [`SILENCE_TIMEOUT`] on the receive side, so the indicator
/// remains lit throughout.
pub const RESEND_INTERVAL: Duration = Duration::from_millis(150);

pub struct EmitDebouncer {
    last_sent: Option<Instant>,
}

impl EmitDebouncer {
    pub fn new() -> Self {
        Self { last_sent: None }
    }

    /// `true` if the caller should transmit a fresh typing event now.
    pub fn should_send(&mut self, now: Instant) -> bool {
        let send = match self.last_sent {
            None => true,
            Some(last) => now.saturating_duration_since(last) >= RESEND_INTERVAL,
        };
        if send {
            self.last_sent = Some(now);
        }
        send
    }

    /// Reset the debouncer so the next call to [`Self::should_send`]
    /// emits regardless of history. Callers use this on focus loss or
    /// content commit so a fresh burst is immediately visible.
    pub fn reset(&mut self) {
        self.last_sent = None;
    }
}

impl Default for EmitDebouncer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_note_is_visible_within_window() {
        let t0 = Instant::now();
        let mut tracker = TypingTracker::new();
        tracker.note("alice", "typing", t0);
        let ind = tracker.active_at(t0 + Duration::from_millis(100));
        assert_eq!(ind.len(), 1);
        assert_eq!(ind[0].user_id, "alice");
        assert_eq!(ind[0].action, "typing");
    }

    #[test]
    fn indicator_clears_after_silence() {
        let t0 = Instant::now();
        let mut tracker = TypingTracker::new();
        tracker.note("alice", "typing", t0);
        let ind = tracker.active_at(t0 + SILENCE_TIMEOUT + Duration::from_millis(50));
        assert!(ind.is_empty());
    }

    #[test]
    fn many_notes_within_window_are_a_single_indicator() {
        let t0 = Instant::now();
        let mut tracker = TypingTracker::new();
        // 100 emits within 300ms
        for i in 0..100 {
            tracker.note("alice", "typing", t0 + Duration::from_millis(i * 3));
        }
        let ind = tracker.active_at(t0 + Duration::from_millis(300));
        assert_eq!(ind.len(), 1);
        assert_eq!(ind[0].action, "typing");
    }

    #[test]
    fn action_switch_replaces_visible_indicator() {
        let t0 = Instant::now();
        let mut tracker = TypingTracker::new();
        tracker.note("alice", "typing", t0);
        tracker.note("alice", "resizing", t0 + Duration::from_millis(10));
        let ind = tracker.active_at(t0 + Duration::from_millis(20));
        assert_eq!(ind.len(), 1);
        assert_eq!(ind[0].action, "resizing");
    }

    #[test]
    fn multiple_peers_show_independently() {
        let t0 = Instant::now();
        let mut tracker = TypingTracker::new();
        tracker.note("alice", "typing", t0);
        tracker.note("bob", "moving", t0);
        let ind = tracker.active_at(t0 + Duration::from_millis(50));
        assert_eq!(ind.len(), 2);
    }

    #[test]
    fn clear_peer_drops_all_of_their_actions() {
        let t0 = Instant::now();
        let mut tracker = TypingTracker::new();
        tracker.note("alice", "typing", t0);
        tracker.note("alice", "resizing", t0);
        tracker.note("bob", "typing", t0);
        tracker.clear_peer("alice");
        let ind = tracker.active_at(t0 + Duration::from_millis(50));
        assert_eq!(ind.len(), 1);
        assert_eq!(ind[0].user_id, "bob");
    }

    #[test]
    fn emit_debouncer_first_send_passes() {
        let mut d = EmitDebouncer::new();
        let t0 = Instant::now();
        assert!(d.should_send(t0));
    }

    #[test]
    fn emit_debouncer_swallows_rapid_repeats() {
        let mut d = EmitDebouncer::new();
        let t0 = Instant::now();
        assert!(d.should_send(t0));
        assert!(!d.should_send(t0 + Duration::from_millis(1)));
        assert!(!d.should_send(t0 + Duration::from_millis(100)));
    }

    #[test]
    fn emit_debouncer_reopens_after_resend_interval() {
        let mut d = EmitDebouncer::new();
        let t0 = Instant::now();
        d.should_send(t0);
        assert!(d.should_send(t0 + RESEND_INTERVAL + Duration::from_millis(1)));
    }

    #[test]
    fn emit_debouncer_reset_forces_next_send() {
        let mut d = EmitDebouncer::new();
        let t0 = Instant::now();
        d.should_send(t0);
        d.reset();
        assert!(d.should_send(t0 + Duration::from_millis(1)));
    }
}
