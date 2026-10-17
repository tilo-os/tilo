//! The live banners and the retained history, with the bookkeeping every
//! host shares: the banner cap, the bounded history, a reversible Clear all
//! and in-place replacement of mirrored notifications.
//!
//! The log performs no effects. Operations that take notifications out hand
//! them back, so the host can answer the sender (a freedesktop signal, a
//! command to the origin device) and decide whether they are retained.

use std::collections::BTreeSet;

use ui::time::{Duration, Instant};

use super::history::{HistoryRow, group_key, history_rows};
use super::model::{ContinuityNotificationId, MAX_HISTORY, MAX_NOTIFICATIONS, Notification};

/// How long Clear all stays reversible.
pub const CLEAR_UNDO_WINDOW: Duration = Duration::from_secs(8);

/// What a banner the person just let go of (the pointer left the stack,
/// keyboard focus went back to their window) has left at least, so it does
/// not start fading the moment they stop holding it.
pub const RELEASE_GRACE: Duration = Duration::from_millis(1500);

#[derive(Debug)]
struct PendingClear {
    notifications: Vec<Notification>,
    deadline: Instant,
}

fn retain_newest(history: &mut Vec<Notification>) {
    let excess = history.len().saturating_sub(MAX_HISTORY);
    if excess != 0 {
        history.drain(..excess);
    }
}

fn clear_deadline(now: Instant) -> Instant {
    now.checked_add(CLEAR_UNDO_WINDOW).unwrap_or(now)
}

#[derive(Debug, Default)]
pub struct NotificationLog {
    /// Oldest-first, capped at [`MAX_NOTIFICATIONS`].
    active: Vec<Notification>,
    /// Oldest-first, bounded to [`MAX_HISTORY`].
    history: Vec<Notification>,
    /// History removed by the most recent Clear all, kept only long enough
    /// to make that destructive-looking action safely reversible.
    pending_clear: Option<PendingClear>,
    /// Set by every history mutation; the host persists and clears it.
    history_dirty: bool,
    /// Whether the person held the banners at the last
    /// [`Self::present`]: letting go grants [`RELEASE_GRACE`].
    held: bool,
}

impl NotificationLog {
    /// Live banners, oldest first.
    pub fn active(&self) -> &[Notification] {
        &self.active
    }

    /// Retained notifications, oldest first.
    pub fn history(&self) -> &[Notification] {
        &self.history
    }

    /// Show `notification` as a banner. Entries pushed past the cap leave
    /// the stack oldest first and are handed back for retirement.
    #[must_use]
    pub fn show(&mut self, notification: Notification) -> Vec<Notification> {
        self.active.push(notification);
        let excess = self.active.len().saturating_sub(MAX_NOTIFICATIONS);
        self.active.drain(..excess).collect()
    }

    /// Take the live banner `id` off the stack.
    pub fn take_active(&mut self, id: u64) -> Option<Notification> {
        self.take_active_where(|n| n.id == id)
    }

    /// Take the first live banner matching `matches` off the stack.
    pub fn take_active_where(
        &mut self,
        matches: impl Fn(&Notification) -> bool,
    ) -> Option<Notification> {
        let pos = self.active.iter().position(matches)?;
        Some(self.active.remove(pos))
    }

    /// Drop every live banner matching `matches` without retiring it.
    pub fn discard_active_where(&mut self, matches: impl Fn(&Notification) -> bool) {
        self.active.retain(|n| !matches(n));
    }

    /// Say which live banners the stack has `on_screen` at `now`, and
    /// whether the person `held` them (the pointer on the stack, keyboard
    /// focus in a banner). A banner's time runs only while it is on screen
    /// and unheld, so one that waited off screen, or that is being read,
    /// keeps all it had; letting go leaves each at least
    /// [`RELEASE_GRACE`].
    pub fn present(&mut self, now: Instant, on_screen: &[u64], held: bool) {
        let released = self.held && !held;
        self.held = held;
        for n in &mut self.active {
            let shown = on_screen.contains(&n.id);
            n.dwell.present(now, shown, held);
            if released && shown {
                n.dwell.grant(n.dismiss_after, now, RELEASE_GRACE);
            }
        }
    }

    /// Whether the person held the banners at the last [`Self::present`].
    pub fn held(&self) -> bool {
        self.held
    }

    /// Take every banner whose dwell has run out at `now`, oldest first.
    pub fn take_expired(&mut self, now: Instant) -> Vec<Notification> {
        if !self.active.iter().any(|n| n.is_expired_at(now)) {
            return Vec::new();
        }
        let (expired, kept) = std::mem::take(&mut self.active)
            .into_iter()
            .partition(|n| n.is_expired_at(now));
        self.active = kept;
        expired
    }

    /// Retain `notification` as the newest history entry.
    pub fn retain(&mut self, notification: Notification) {
        self.history.push(notification);
        retain_newest(&mut self.history);
        self.history_dirty = true;
    }

    /// The retained notification `id`.
    pub fn history_entry(&self, id: u64) -> Option<&Notification> {
        self.history.iter().find(|n| n.id == id)
    }

    /// Remove the retained notification `id`.
    pub fn take_history(&mut self, id: u64) -> Option<Notification> {
        let pos = self.history.iter().position(|n| n.id == id)?;
        self.history_dirty = true;
        Some(self.history.remove(pos))
    }

    /// Ids of every retained notification in the app group `key`.
    pub fn group_ids(&self, key: &str) -> Vec<u64> {
        self.history
            .iter()
            .filter(|n| group_key(n).as_deref() == Some(key))
            .map(|n| n.id)
            .collect()
    }

    /// Rows of the notification centre, newest first, grouped by verified
    /// app. `collapsed` names the group keys the viewer folded.
    pub fn rows(&self, collapsed: &BTreeSet<String>) -> Vec<HistoryRow> {
        history_rows(&self.history, collapsed)
    }

    /// Clear the history, keeping it restorable for [`CLEAR_UNDO_WINDOW`].
    /// Returns whether anything was cleared.
    pub fn clear_history(&mut self, now: Instant) -> bool {
        if self.history.is_empty() {
            return false;
        }
        self.history_dirty = true;
        let mut removed = std::mem::take(&mut self.history);
        if let Some(pending) = self.pending_clear.as_mut() {
            pending.notifications.append(&mut removed);
            retain_newest(&mut pending.notifications);
            pending.deadline = clear_deadline(now);
        } else {
            self.pending_clear = Some(PendingClear {
                notifications: removed,
                deadline: clear_deadline(now),
            });
        }
        true
    }

    /// Whether Clear all can still be undone.
    pub fn can_undo_clear(&self) -> bool {
        self.pending_clear.is_some()
    }

    /// When the pending Clear all stops being reversible.
    pub fn clear_deadline(&self) -> Option<Instant> {
        self.pending_clear.as_ref().map(|pending| pending.deadline)
    }

    /// Restore the cleared oldest-first snapshot ahead of notifications that
    /// arrived during the undo window, preserving chronological order.
    pub fn undo_clear(&mut self) -> bool {
        let Some(mut pending) = self.pending_clear.take() else {
            return false;
        };
        pending.notifications.append(&mut self.history);
        retain_newest(&mut pending.notifications);
        self.history = pending.notifications;
        self.history_dirty = true;
        true
    }

    /// Expire the undo snapshot. Returns true only on the clearing edge.
    pub fn expire_clear(&mut self, now: Instant) -> bool {
        let expired = self
            .pending_clear
            .as_ref()
            .is_some_and(|pending| now >= pending.deadline);
        if expired {
            self.pending_clear = None;
        }
        expired
    }

    /// Put history restored from an earlier session ahead of anything this
    /// session already retained, keeping the newest bound.
    pub fn restore_history(&mut self, mut restored: Vec<Notification>) {
        let had_session_entries = !self.history.is_empty();
        restored.append(&mut self.history);
        retain_newest(&mut restored);
        self.history = restored;
        self.history_dirty = had_session_entries;
    }

    /// Whether the history changed since the last call; clears the flag.
    pub fn take_history_dirty(&mut self) -> bool {
        std::mem::take(&mut self.history_dirty)
    }

    /// Replace the live banner or retained entry mirrored from the same
    /// origin as `replacement`, keeping the local id stable so hit targets
    /// and centre selection survive. Returns whether one was replaced.
    pub fn replace_remote(&mut self, replacement: &Notification) -> bool {
        let Some(route) = replacement.remote_route.as_ref() else {
            return false;
        };
        let same_origin = |n: &&mut Notification| n.remote_route.as_ref() == Some(route);
        if let Some(existing) = self.active.iter_mut().find(same_origin) {
            let (id, dwell) = (existing.id, existing.dwell);
            *existing = replacement.clone();
            existing.id = id;
            // The banner the person is reading stays where it is in its
            // time on screen, rather than sliding in again.
            existing.dwell = dwell;
            return true;
        }
        if let Some(existing) = self.history.iter_mut().find(same_origin) {
            let id = existing.id;
            *existing = replacement.clone();
            existing.id = id;
            self.history_dirty = true;
            return true;
        }
        false
    }

    /// Forget every copy of a mirrored notification its origin retired:
    /// live, retained and in a pending Clear all. Returns whether anything
    /// changed.
    pub fn retire_remote(&mut self, source_device: &str, id: ContinuityNotificationId) -> bool {
        let matches = |n: &Notification| {
            n.remote_route.as_ref().is_some_and(|route| {
                route.source_device == source_device && route.notification_id == id
            })
        };
        let pending_len = |log: &Self| {
            log.pending_clear
                .as_ref()
                .map_or(0, |pending| pending.notifications.len())
        };
        let before = (self.active.len(), self.history.len(), pending_len(self));
        self.active.retain(|n| !matches(n));
        self.history.retain(|n| !matches(n));
        if let Some(pending) = self.pending_clear.as_mut() {
            pending.notifications.retain(|n| !matches(n));
        }
        let after = (self.active.len(), self.history.len(), pending_len(self));
        if after.1 != before.1 {
            self.history_dirty = true;
        }
        after != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(notifications: &[Notification]) -> Vec<&str> {
        notifications.iter().map(|n| n.title.as_str()).collect()
    }

    #[test]
    fn the_stack_caps_banners_and_hands_back_the_oldest() {
        let mut log = NotificationLog::default();
        let mut evicted = Vec::new();
        for i in 0..(MAX_NOTIFICATIONS + 3) {
            evicted.extend(log.show(Notification::new(format!("n{i}"), "")));
        }
        assert_eq!(log.active().len(), MAX_NOTIFICATIONS);
        assert_eq!(log.active()[0].title, "n3");
        assert_eq!(titles(&evicted), ["n0", "n1", "n2"]);
    }

    #[test]
    fn expiry_takes_only_elapsed_banners() {
        let mut log = NotificationLog::default();
        let now = ui::time::now();
        let quick = Notification::new("quick", "").with_dismiss_after(Duration::from_millis(50));
        let slow = Notification::new("slow", "");
        let shown = [quick.id, slow.id];
        let _ = log.show(quick);
        let _ = log.show(slow);
        log.present(now, &shown, false);
        let expired = log.take_expired(now + Duration::from_millis(200));
        assert_eq!(titles(&expired), ["quick"]);
        assert_eq!(titles(log.active()), ["slow"]);
    }

    /// A banner ages only on screen and unheld: one that did not fit, or
    /// one under the pointer, outlives its dismiss window; letting go of
    /// it leaves a moment before it leaves.
    #[test]
    fn banners_age_only_on_screen_and_unheld() {
        let mut log = NotificationLog::default();
        let t0 = ui::time::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let seen = Notification::new("seen", "");
        let unseen = Notification::new("unseen", "");
        let seen_id = seen.id;
        let _ = log.show(unseen);
        let _ = log.show(seen);
        log.present(t0, &[seen_id], false);
        assert!(log.take_expired(at(4_000)).is_empty());
        log.present(at(4_000), &[seen_id], true);
        assert!(log.held());
        assert!(
            log.take_expired(at(60_000)).is_empty(),
            "a banner under the pointer never leaves"
        );
        log.present(at(60_000), &[seen_id], false);
        assert!(log.take_expired(at(60_000 + 1_400)).is_empty());
        assert_eq!(
            titles(&log.take_expired(at(60_000 + 1_500))),
            ["seen"],
            "let go, it leaves after the grace"
        );
        assert_eq!(
            titles(log.active()),
            ["unseen"],
            "a banner never on screen never expires"
        );
    }

    #[test]
    fn clear_undo_restores_exact_order_before_new_arrivals() {
        let mut log = NotificationLog::default();
        let now = ui::time::now();
        log.retain(Notification::new("first", "one"));
        log.retain(Notification::new("second", "two"));
        assert!(log.clear_history(now));
        log.retain(Notification::new("new", "three"));
        assert!(log.undo_clear());
        assert_eq!(titles(log.history()), ["first", "second", "new"]);
        assert!(!log.undo_clear());
    }

    #[test]
    fn a_clear_stays_reversible_for_eight_seconds() {
        let mut log = NotificationLog::default();
        let now = ui::time::now();
        log.retain(Notification::new("one", "body"));
        assert!(log.clear_history(now));
        assert!(!log.expire_clear(now + Duration::from_secs(7)));
        assert!(log.can_undo_clear());
        assert!(log.expire_clear(now + Duration::from_secs(8)));
        assert!(!log.can_undo_clear());
        assert!(!log.undo_clear());
    }

    #[test]
    fn a_second_clear_extends_one_recoverable_snapshot() {
        let mut log = NotificationLog::default();
        let now = ui::time::now();
        log.retain(Notification::new("before", "body"));
        assert!(log.clear_history(now));
        log.retain(Notification::new("during", "body"));
        assert!(log.clear_history(now + Duration::from_secs(4)));
        assert!(!log.expire_clear(now + Duration::from_secs(8)));
        assert!(log.undo_clear());
        assert_eq!(titles(log.history()), ["before", "during"]);
    }

    #[test]
    fn history_is_bounded_and_restored_entries_go_first() {
        let mut log = NotificationLog::default();
        for i in 0..(MAX_HISTORY + 5) {
            log.retain(Notification::new(format!("h{i}"), ""));
        }
        assert_eq!(log.history().len(), MAX_HISTORY);
        assert_eq!(log.history()[0].title, "h5");
        assert!(log.take_history_dirty());
        log.restore_history(vec![Notification::new("old", "")]);
        assert_eq!(log.history().len(), MAX_HISTORY);
        assert_eq!(
            log.history().last().unwrap().title,
            format!("h{}", MAX_HISTORY + 4)
        );
        assert!(
            log.take_history_dirty(),
            "session entries must be written back"
        );
    }
}
