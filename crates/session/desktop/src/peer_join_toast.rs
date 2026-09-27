//! Batching queue for peer join / leave toasts.
//!
//! When a burst of peers joins a workspace simultaneously (e.g. an
//! invite link is shared to a group chat and three teammates click at
//! the same moment), we don't want three overlapping toasts. This
//! module buffers arrival/departure events for [`BATCH_WINDOW`] and
//! emits at most one combined toast per event kind.
//!
//! The batcher is time-source-injectable and pure — the caller pulls
//! ready messages via `drain_ready` on a timer and pushes them into
//! whatever notification surface the host prefers.

use std::collections::VecDeque;
use ui::clock::{Duration, Instant};

use crate::i18n::t_args;

/// Events within this window since the first entry are folded into a
/// single toast when drained.
pub const BATCH_WINDOW: Duration = Duration::from_secs(1);

/// Direction of the event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerEventKind {
    Joined,
    Left,
}

/// A single "peer joined/left" event queued for batching.
#[derive(Clone, Debug)]
struct QueuedEvent {
    kind: PeerEventKind,
    /// Friendly name (falls back to `user_id` if unknown).
    name: String,
    t: Instant,
}

impl QueuedEvent {
    /// Copy just the timestamp — plain `.copied()` would require `Copy`
    /// on the whole struct, which we can't derive because of `name`.
    fn time(&self) -> Instant {
        self.t
    }
}

/// A message ready to be shown as a toast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyToast {
    pub kind: PeerEventKind,
    pub title: String,
    pub body: String,
}

/// Time-injectable batcher.
#[derive(Default)]
pub struct PeerToastQueue {
    pending: VecDeque<QueuedEvent>,
}

impl PeerToastQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue a fresh event.
    pub fn push(&mut self, kind: PeerEventKind, name: impl Into<String>, t: Instant) {
        self.pending.push_back(QueuedEvent {
            kind,
            name: name.into(),
            t,
        });
    }

    /// Drain all events whose batch window has closed at instant `now`,
    /// grouped by kind, and format the toast(s).
    pub fn drain_ready(&mut self, now: Instant) -> Vec<ReadyToast> {
        // Measured from the first event, not the last, so a slow trickle
        // of joins over >1s still produces separate toasts once per second.
        let mut out: Vec<ReadyToast> = Vec::new();
        while let Some(head_t) = self.pending.front().map(QueuedEvent::time) {
            let elapsed = now.saturating_duration_since(head_t);
            if elapsed < BATCH_WINDOW {
                break;
            }
            // `Instant::checked_add` can't panic; a batch window that
            // (unreachably, for a 1-second constant) overflowed just
            // closes the batch at `head_t` instead.
            let boundary = head_t.checked_add(BATCH_WINDOW).unwrap_or(head_t);
            let mut batch = Vec::new();
            while let Some(front) = self.pending.front() {
                if front.t <= boundary {
                    let Some(event) = self.pending.pop_front() else {
                        break;
                    };
                    batch.push(event);
                } else {
                    break;
                }
            }
            let joins: Vec<_> = batch
                .iter()
                .filter(|e| e.kind == PeerEventKind::Joined)
                .collect();
            let leaves: Vec<_> = batch
                .iter()
                .filter(|e| e.kind == PeerEventKind::Left)
                .collect();
            if !joins.is_empty() {
                out.push(format_batch(PeerEventKind::Joined, &joins));
            }
            if !leaves.is_empty() {
                out.push(format_batch(PeerEventKind::Left, &leaves));
            }
        }
        out
    }

    /// Force-flush every pending event (e.g. on shutdown).
    pub fn flush(&mut self) -> Vec<ReadyToast> {
        let all: Vec<_> = self.pending.drain(..).collect();
        let joins: Vec<_> = all
            .iter()
            .filter(|e| e.kind == PeerEventKind::Joined)
            .collect();
        let leaves: Vec<_> = all
            .iter()
            .filter(|e| e.kind == PeerEventKind::Left)
            .collect();
        let mut out = Vec::new();
        if !joins.is_empty() {
            out.push(format_batch(PeerEventKind::Joined, &joins));
        }
        if !leaves.is_empty() {
            out.push(format_batch(PeerEventKind::Left, &leaves));
        }
        out
    }

    /// Peek the number of queued events (useful for tests and metrics).
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

fn format_batch(kind: PeerEventKind, batch: &[&QueuedEvent]) -> ReadyToast {
    debug_assert!(!batch.is_empty());
    let (one, many) = match kind {
        PeerEventKind::Joined => ("peer-toast-joined-one", "peer-toast-joined-many"),
        PeerEventKind::Left => ("peer-toast-left-one", "peer-toast-left-many"),
    };
    // Dedup so a peer that flapped in a single window only counts once.
    let mut names: Vec<String> = batch.iter().map(|e| e.name.clone()).collect();
    names.sort();
    names.dedup();
    let name_at = |i: usize| names.get(i).map_or("", String::as_str);
    match names.len() {
        1 => ReadyToast {
            kind,
            title: t_args(one, &[("name", name_at(0).into())]),
            body: String::new(),
        },
        n if n <= 3 => ReadyToast {
            kind,
            title: t_args(many, &[("count", n.into())]),
            body: names.join(", "),
        },
        n => ReadyToast {
            kind,
            title: t_args(many, &[("count", n.into())]),
            body: t_args(
                "peer-toast-overflow",
                &[
                    ("first", name_at(0).into()),
                    ("second", name_at(1).into()),
                    ("third", name_at(2).into()),
                    ("rest", n.saturating_sub(3).into()),
                ],
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_join_toasts_batch_dedupe_split_and_overflow_within_the_window() {
        fn single_peer_joined_produces_named_toast() {
            let mut q = PeerToastQueue::new();
            let t0 = Instant::now();
            q.push(PeerEventKind::Joined, "quinten", t0);
            let ready = q.drain_ready(t0 + BATCH_WINDOW + Duration::from_millis(1));
            assert_eq!(ready.len(), 1);
            assert_eq!(ready[0].kind, PeerEventKind::Joined);
            assert_eq!(ready[0].title, "quinten joined");
        }

        fn three_joins_within_window_are_one_toast() {
            let mut q = PeerToastQueue::new();
            let t0 = Instant::now();
            q.push(PeerEventKind::Joined, "alice", t0);
            q.push(
                PeerEventKind::Joined,
                "bob",
                t0 + Duration::from_millis(100),
            );
            q.push(
                PeerEventKind::Joined,
                "charlie",
                t0 + Duration::from_millis(300),
            );
            let ready = q.drain_ready(t0 + BATCH_WINDOW + Duration::from_millis(1));
            assert_eq!(ready.len(), 1);
            assert_eq!(ready[0].title, "3 peers joined");
            assert_eq!(ready[0].body, "alice, bob, charlie");
        }

        fn join_and_leave_split_into_two_toasts() {
            let mut q = PeerToastQueue::new();
            let t0 = Instant::now();
            q.push(PeerEventKind::Joined, "alice", t0);
            q.push(PeerEventKind::Left, "bob", t0 + Duration::from_millis(100));
            let ready = q.drain_ready(t0 + BATCH_WINDOW + Duration::from_millis(1));
            assert_eq!(ready.len(), 2);
            assert!(ready.iter().any(|r| r.title == "alice joined"));
            assert!(ready.iter().any(|r| r.title == "bob left"));
        }

        fn events_arriving_after_batch_close_stay_pending() {
            let mut q = PeerToastQueue::new();
            let t0 = Instant::now();
            q.push(PeerEventKind::Joined, "alice", t0);
            // A second batch begins after the first window ends.
            q.push(
                PeerEventKind::Joined,
                "bob",
                t0 + BATCH_WINDOW + Duration::from_millis(500),
            );
            let ready = q.drain_ready(t0 + BATCH_WINDOW + Duration::from_millis(10));
            assert_eq!(ready.len(), 1);
            assert_eq!(ready[0].title, "alice joined");
            assert_eq!(q.pending(), 1);
        }

        fn deduplicates_flapping_peer_within_window() {
            let mut q = PeerToastQueue::new();
            let t0 = Instant::now();
            q.push(PeerEventKind::Joined, "alice", t0);
            q.push(
                PeerEventKind::Joined,
                "alice",
                t0 + Duration::from_millis(500),
            );
            let ready = q.drain_ready(t0 + BATCH_WINDOW + Duration::from_millis(1));
            assert_eq!(ready.len(), 1);
            assert_eq!(ready[0].title, "alice joined");
        }

        fn more_than_three_shows_overflow() {
            let mut q = PeerToastQueue::new();
            let t0 = Instant::now();
            for name in ["a", "b", "c", "d", "e"] {
                q.push(PeerEventKind::Joined, name, t0);
            }
            let ready = q.drain_ready(t0 + BATCH_WINDOW + Duration::from_millis(1));
            assert_eq!(ready.len(), 1);
            assert_eq!(ready[0].title, "5 peers joined");
            assert!(ready[0].body.ends_with("+2 more"));
        }

        fn flush_forces_pending_out_immediately() {
            let mut q = PeerToastQueue::new();
            let t0 = Instant::now();
            q.push(PeerEventKind::Joined, "alice", t0);
            assert!(q.drain_ready(t0).is_empty());
            let flushed = q.flush();
            assert_eq!(flushed.len(), 1);
        }

        single_peer_joined_produces_named_toast();
        three_joins_within_window_are_one_toast();
        join_and_leave_split_into_two_toasts();
        events_arriving_after_batch_close_stay_pending();
        deduplicates_flapping_peer_within_window();
        more_than_three_shows_overflow();
        flush_forces_pending_out_immediately();
    }
}
