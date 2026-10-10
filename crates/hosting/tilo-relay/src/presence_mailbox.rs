//! Presence on its way to one connection: coalesced, paced and batched
//! (TIL-710).
//!
//! Every room a connection is subscribed to posts the presence it fans out
//! into the connection's [`PresenceMailbox`] instead of its frame queue, and
//! the connection's writer takes what is due as one batch per room.
//!
//! - **Latest state wins.** Presence with a slot (a pointer, a caret, a
//!   selection, the range being typed; `neighbour_wire::PresenceSlot`)
//!   replaces the pending presence of the same author in the same slot, so a
//!   slow or busy connection receives where things are, not a backlog of where
//!   they were — and never loses the last caret position to a full queue.
//! - **Pointers at most every [`MOTION_GAP`] (20 Hz) per slot.** Receivers
//!   glide a peer's pointer over the gap between two samples
//!   (`ui::presence::POINTER_EASE_MAX` caps it at 100 ms), so 20 Hz moves as
//!   smoothly as the 30 Hz the shell publishes, for a third fewer frames. The
//!   first move after a pause goes out within [`BATCH_WINDOW`].
//! - **Carets, selections and typing on every change**, after at most
//!   [`BATCH_WINDOW`] (5 ms), which is what lets a keystroke's `Typing` and
//!   `Caret` travel as one frame.
//! - **Everything else in order.** Unslotted presence (heartbeats, `SyncReq`,
//!   arrivals, identity) is never coalesced, and is delivered after the
//!   presence posted before it: when one is due, everything pending goes with
//!   it, in posting order.
//!
//! Each payload is shared, not copied, between the connections it is posted
//! to. Deltas never pass through here, so edit delivery never waits for
//! presence.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tilo_delivery_protocol::transport_v2::PresenceSlotKey;
use tilo_peer_state::PresenceSlot;
use tokio::sync::Notify;
use tokio::time::Instant;

/// The shortest gap between two moves of one pointer slot to one connection.
pub const MOTION_GAP: Duration = Duration::from_millis(50);
/// How long presence waits for more to travel in the same frame.
pub const BATCH_WINDOW: Duration = Duration::from_millis(5);
/// Pending slots, and separately pending unslotted presence, one connection
/// holds at most; beyond it new presence is dropped, as a full queue drops it.
const MAX_PENDING: usize = 1024;

/// One presence payload the writer is to send now.
#[derive(Debug)]
pub struct DuePresence {
    /// The room it was fanned out in.
    pub workspace_id: u64,
    /// The slot it codes against on a batching connection, or `None` for
    /// presence that travels whole.
    pub key: Option<PresenceSlotKey>,
    /// The author-stamped `AttributedPresence` bytes, shared by every
    /// connection it was posted to.
    pub payload: Arc<[u8]>,
}

/// The presence pending for one connection.
#[derive(Default)]
pub struct PresenceMailbox {
    state: Mutex<State>,
    notify: Notify,
}

#[derive(Default)]
struct State {
    slotted: HashMap<PresenceSlotKey, Pending>,
    events: VecDeque<Event>,
    /// When each motion slot last went out, for [`MOTION_GAP`].
    sent: HashMap<PresenceSlotKey, Instant>,
    seq: u64,
    /// The deadline the writer is waiting for, if it is waiting.
    armed: Option<Instant>,
}

struct Pending {
    payload: Arc<[u8]>,
    motion: bool,
    /// When the slot became pending.
    since: Instant,
    /// Posting order of the newest payload.
    seq: u64,
}

struct Event {
    workspace_id: u64,
    payload: Arc<[u8]>,
    since: Instant,
    seq: u64,
}

/// `at + by`, or `at` itself past the end of time.
fn later(at: Instant, by: Duration) -> Instant {
    at.checked_add(by).unwrap_or(at)
}

/// When presence pending since `since` in slot `key` falls due.
fn due_at(
    sent: &HashMap<PresenceSlotKey, Instant>,
    key: &PresenceSlotKey,
    since: Instant,
    motion: bool,
) -> Instant {
    let batched = later(since, BATCH_WINDOW);
    if !motion {
        return batched;
    }
    sent.get(key)
        .map_or(batched, |sent| batched.max(later(*sent, MOTION_GAP)))
}

impl State {
    fn next_due(&self) -> Option<Instant> {
        let event = self
            .events
            .front()
            .map(|event| later(event.since, BATCH_WINDOW));
        let slotted = self
            .slotted
            .iter()
            .map(|(key, pending)| due_at(&self.sent, key, pending.since, pending.motion))
            .min();
        match (event, slotted) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

impl PresenceMailbox {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Post `payload`, which `author` sent to room `workspace_id` in `slot`
    /// (or unslotted). Returns whether it was taken: only a mailbox at its
    /// bound refuses.
    pub fn post(
        &self,
        workspace_id: u64,
        author: [u8; 32],
        slot: Option<PresenceSlot>,
        payload: Arc<[u8]>,
        now: Instant,
    ) -> bool {
        let mut state = self.state();
        state.seq = state.seq.saturating_add(1);
        let seq = state.seq;
        let due = if let Some(slot) = slot {
            let key = PresenceSlotKey {
                workspace_id,
                author,
                slot: slot.id,
            };
            if !state.slotted.contains_key(&key) && state.slotted.len() >= MAX_PENDING {
                return false;
            }
            let pending = state.slotted.entry(key).or_insert_with(|| Pending {
                payload: Arc::clone(&payload),
                motion: slot.motion,
                since: now,
                seq,
            });
            pending.payload = payload;
            pending.motion = slot.motion;
            pending.seq = seq;
            let since = pending.since;
            due_at(&state.sent, &key, since, slot.motion)
        } else {
            if state.events.len() >= MAX_PENDING {
                return false;
            }
            state.events.push_back(Event {
                workspace_id,
                payload,
                since: now,
                seq,
            });
            later(now, BATCH_WINDOW)
        };
        let wake = state.armed.is_none_or(|armed| due < armed);
        drop(state);
        if wake {
            self.notify.notify_one();
        }
        true
    }

    /// When the earliest pending presence falls due, if any is pending.
    pub fn next_due(&self) -> Option<Instant> {
        self.state().next_due()
    }

    /// Wait until some presence is due. Cancel-safe: nothing is taken.
    pub async fn ready(&self) {
        loop {
            let next = {
                let mut state = self.state();
                let next = state.next_due();
                state.armed = next;
                next
            };
            match next {
                Some(at) if at <= Instant::now() => return,
                Some(at) => {
                    tokio::select! {
                        () = tokio::time::sleep_until(at) => return,
                        () = self.notify.notified() => {}
                    }
                }
                None => self.notify.notified().await,
            }
        }
    }

    /// Take everything due at `now`, in posting order. When unslotted
    /// presence is due, everything pending is taken with it, so it never
    /// overtakes presence posted before it.
    pub fn take_due(&self, now: Instant) -> Vec<DuePresence> {
        let mut state = self.state();
        state.armed = None;
        let everything = state
            .events
            .front()
            .is_some_and(|event| later(event.since, BATCH_WINDOW) <= now);
        let keys: Vec<PresenceSlotKey> = state
            .slotted
            .iter()
            .filter(|(key, pending)| {
                everything || due_at(&state.sent, key, pending.since, pending.motion) <= now
            })
            .map(|(key, _)| *key)
            .collect();
        let mut due: Vec<(u64, DuePresence)> = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(pending) = state.slotted.remove(&key) {
                if pending.motion {
                    state.sent.insert(key, now);
                }
                due.push((
                    pending.seq,
                    DuePresence {
                        workspace_id: key.workspace_id,
                        key: Some(key),
                        payload: pending.payload,
                    },
                ));
            }
        }
        while let Some(event) = state
            .events
            .pop_front_if(|event| everything || later(event.since, BATCH_WINDOW) <= now)
        {
            due.push((
                event.seq,
                DuePresence {
                    workspace_id: event.workspace_id,
                    key: None,
                    payload: event.payload,
                },
            ));
        }
        // A pointer that went out a gap ago or longer paces nothing.
        state
            .sent
            .retain(|_, sent| now.saturating_duration_since(*sent) < MOTION_GAP);
        drop(state);
        due.sort_by_key(|(seq, _)| *seq);
        due.into_iter().map(|(_, presence)| presence).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOM: u64 = 7;
    const ALICE: [u8; 32] = [1; 32];
    const BOB: [u8; 32] = [2; 32];

    fn pointer(id: u64) -> PresenceSlot {
        PresenceSlot { id, motion: true }
    }

    fn caret(id: u64) -> PresenceSlot {
        PresenceSlot { id, motion: false }
    }

    fn bytes(text: &str) -> Arc<[u8]> {
        Arc::from(text.as_bytes().to_vec())
    }

    fn texts(due: &[DuePresence]) -> Vec<String> {
        due.iter()
            .map(|presence| String::from_utf8_lossy(&presence.payload).into_owned())
            .collect()
    }

    #[test]
    fn the_latest_state_of_each_slot_goes_out_once_the_window_passes() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        assert!(mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("caret 1"), t0));
        assert!(mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("caret 2"), t0));
        assert!(mailbox.post(ROOM, ALICE, Some(caret(2)), bytes("typing"), t0));
        assert!(mailbox.post(ROOM, BOB, Some(caret(1)), bytes("bob caret"), t0));
        assert!(mailbox.take_due(t0).is_empty(), "the batch window is open");
        let due = mailbox.take_due(t0 + BATCH_WINDOW);
        assert_eq!(texts(&due), ["caret 2", "typing", "bob caret"]);
        assert!(due.iter().all(|presence| presence.key.is_some()));
        assert!(mailbox.take_due(t0 + BATCH_WINDOW).is_empty());
        assert_eq!(mailbox.next_due(), None);
    }

    #[test]
    fn a_moving_pointer_goes_out_at_most_once_per_gap() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        let mut sent = Vec::new();
        // The shell's 30 Hz for one second, drained as the writer drains it:
        // whenever something falls due.
        let posts: Vec<Instant> = (0..30u32)
            .map(|step| t0 + Duration::from_millis(33) * step)
            .collect();
        for (step, at) in posts.iter().enumerate() {
            mailbox.post(
                ROOM,
                ALICE,
                Some(pointer(1)),
                bytes(&format!("move {step}")),
                *at,
            );
            let next_post = posts
                .get(step + 1)
                .copied()
                .unwrap_or(t0 + Duration::from_secs(2));
            while let Some(due) = mailbox.next_due().filter(|due| *due < next_post) {
                sent.extend(texts(&mailbox.take_due(due)));
            }
        }
        assert!(
            (18..=21).contains(&sent.len()),
            "{} pointer frames in a second: {sent:?}",
            sent.len()
        );
        assert_eq!(sent.first().map(String::as_str), Some("move 0"));
        assert_eq!(
            sent.last().map(String::as_str),
            Some("move 29"),
            "the latest position always arrives"
        );
    }

    #[test]
    fn unslotted_presence_keeps_its_order_and_brings_what_came_before() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        // A pointer that has just moved is held for its gap...
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 1"), t0);
        assert_eq!(texts(&mailbox.take_due(t0 + BATCH_WINDOW)), ["move 1"]);
        let t1 = t0 + Duration::from_millis(10);
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 2"), t1);
        // ...until an event follows it, which must not overtake it.
        mailbox.post(ROOM, ALICE, None, bytes("cleared"), t1);
        mailbox.post(ROOM, BOB, None, bytes("heartbeat"), t1);
        let due = mailbox.take_due(t1 + BATCH_WINDOW);
        assert_eq!(texts(&due), ["move 2", "cleared", "heartbeat"]);
        assert!(due[1].key.is_none());
    }

    #[test]
    fn a_full_mailbox_refuses_new_presence_but_still_updates_its_slots() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        let max = u64::try_from(MAX_PENDING).unwrap();
        for slot in 0..max {
            assert!(mailbox.post(ROOM, ALICE, Some(caret(slot)), bytes("x"), t0));
        }
        assert!(!mailbox.post(ROOM, ALICE, Some(caret(max)), bytes("x"), t0));
        assert!(mailbox.post(ROOM, ALICE, Some(caret(0)), bytes("newer"), t0));
        for _ in 0..MAX_PENDING {
            assert!(mailbox.post(ROOM, ALICE, None, bytes("event"), t0));
        }
        assert!(!mailbox.post(ROOM, ALICE, None, bytes("event"), t0));
        let due = mailbox.take_due(t0 + BATCH_WINDOW);
        assert_eq!(due.len(), MAX_PENDING * 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_writer_wakes_when_presence_falls_due() {
        let mailbox = Arc::new(PresenceMailbox::default());
        let waiter = tokio::spawn({
            let mailbox = mailbox.clone();
            async move {
                mailbox.ready().await;
                Instant::now()
            }
        });
        tokio::task::yield_now().await;
        let posted = Instant::now();
        mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("caret"), posted);
        let woke = waiter.await.unwrap();
        assert!(woke >= posted + BATCH_WINDOW);
        assert_eq!(texts(&mailbox.take_due(woke)), ["caret"]);
    }
}
