//! Presence on its way to one connection: coalesced, paced and batched.
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
//!   smoothly as the 30 Hz the shell publishes, for a third fewer frames. A
//!   move goes as soon as its pace allows, without waiting [`BATCH_WINDOW`]
//!   for company; whatever is pending rides with it. Current clients already
//!   write each pointer slot at most this often (the session writer's pointer
//!   pacing in `tilo_relay_session::client`), so this pace only bites on
//!   older clients and on jitter. The gap is counted from when the previous
//!   move was due, not from when the writer got to it, so a late write does
//!   not hold every later move of a paced client longer.
//! - **Carets, selections and typing on every change**, after at most
//!   [`BATCH_WINDOW`] (5 ms), which is what lets a keystroke's `Typing` and
//!   `Caret` travel as one frame.
//! - **Everything else in order.** Unslotted presence (heartbeats, `SyncReq`,
//!   arrivals, identity) is never coalesced, and is delivered after the
//!   presence posted before it: when one is due, everything pending goes with
//!   it, in posting order.
//! - **Presence rides along.** A 27-byte batch costs about 35 bytes
//!   of QUIC packet on top, so presence that may go early goes with whatever
//!   the connection writes next: a delta, or a batch that fell due
//!   ([`PresenceMailbox::take`] with `ride`). Carets, selections, typing and
//!   events may always ride (their window only exists to share a frame); a
//!   pointer rides once [`RIDE_GAP`] has passed since it last went, so it is
//!   never sent later than its pace allows and never more often than twice
//!   that pace.
//! - **A keystroke's presence waits for its delta.** The toolkit sends a
//!   keystroke's delta and then its `Typing` and `Caret`; the relay admits the
//!   delta and fans the presence out while the delta is still being
//!   committed. Caret, selection and typing posted while a delta of the same
//!   author is still unpublished
//!   (`WorkspaceRuntime::note_delta`) are posted *held*: due after
//!   [`DELTA_HOLD`] rather than [`BATCH_WINDOW`], and taken with the delta
//!   when it is written, so the caret arrives with the character it follows,
//!   in one packet. The delta itself never waits.
//!
//! Each payload is shared, not copied, between the connections it is posted
//! to. Deltas never pass through here, so edit delivery never waits for
//! presence.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use neighbour_wire::PresenceSlot;
use tilo_delivery_protocol::transport_v2::{MAX_SLOTTED_PRESENCE_BYTES, PresenceSlotKey};
use tokio::sync::Notify;
use tokio::time::Instant;

/// The shortest gap between two moves of one pointer slot to one connection:
/// the pointer gap clients pace their own writes to
/// ([`crate::coedit::POINTER_GAP`]), so the two cannot drift apart.
pub const MOTION_GAP: Duration = crate::coedit::POINTER_GAP;
/// How long presence waits for more to travel in the same frame.
pub const BATCH_WINDOW: Duration = Duration::from_millis(5);
/// The shortest gap after which a pointer slot may ride along with
/// something else written to its connection.
pub const RIDE_GAP: Duration = Duration::from_millis(25);
/// The longest a held caret, selection or typing range waits for the delta
/// of the same author it follows.
pub const DELTA_HOLD: Duration = Duration::from_millis(20);
/// Pending slots one connection holds at most; a new slot beyond it is
/// dropped, as a full queue drops presence. A slot's newer value always fits.
const MAX_PENDING: usize = 1024;
/// Pending slots of one author one connection holds at most, so one
/// collaborator inventing slots cannot crowd out everybody else's.
const MAX_SLOTS_PER_AUTHOR: usize = 64;
/// Pending unslotted presence one connection holds at most — the frame queue
/// it replaces held as many. Unslotted presence can be large (a shared
/// window's video frame) and is never coalesced, so a slow reader drops it
/// rather than falling behind.
const MAX_PENDING_EVENTS: usize = 32;

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
    /// Waits for a delta of the same author (never a motion slot).
    held: bool,
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

/// When `pending` presence in slot `key` falls due.
fn due_at(
    sent: &HashMap<PresenceSlotKey, Instant>,
    key: &PresenceSlotKey,
    pending: &Pending,
) -> Instant {
    let window = if pending.held {
        DELTA_HOLD
    } else {
        BATCH_WINDOW
    };
    if !pending.motion {
        return later(pending.since, window);
    }
    // A pointer goes as soon as its pace allows: it does not wait the batch
    // window for company, which would add to every move of a client that
    // already sends at this pace. What is pending rides with it.
    sent.get(key).map_or(pending.since, |sent| {
        pending.since.max(later(*sent, MOTION_GAP))
    })
}

/// Whether `pending` presence in slot `key` may go now with something else:
/// a change slot always, a pointer once [`RIDE_GAP`] has passed since it
/// last went (`sent` forgets a pointer [`MOTION_GAP`] after it went).
fn may_ride(
    sent: &HashMap<PresenceSlotKey, Instant>,
    key: &PresenceSlotKey,
    pending: &Pending,
    now: Instant,
) -> bool {
    !pending.motion
        || sent
            .get(key)
            .is_none_or(|sent| now.saturating_duration_since(*sent) >= RIDE_GAP)
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
            .map(|(key, pending)| due_at(&self.sent, key, pending))
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
    /// (or unslotted). `held` marks presence that follows a delta of the
    /// same author: a caret, selection or typing range then waits up to
    /// [`DELTA_HOLD`] to go with that delta. Returns whether it was taken:
    /// only a mailbox at its bound refuses.
    pub fn post(
        &self,
        workspace_id: u64,
        author: [u8; 32],
        slot: Option<PresenceSlot>,
        payload: Arc<[u8]>,
        now: Instant,
        held: bool,
    ) -> bool {
        let mut state = self.state();
        state.seq = state.seq.saturating_add(1);
        let seq = state.seq;
        // Only small latest-state presence is held by slot; anything larger
        // is delivered, and bounded, like an event.
        let slot = slot.filter(|_| payload.len() <= MAX_SLOTTED_PRESENCE_BYTES);
        let due = if let Some(slot) = slot {
            let key = PresenceSlotKey {
                workspace_id,
                author,
                slot: slot.id,
            };
            if !state.slotted.contains_key(&key)
                && (state.slotted.len() >= MAX_PENDING
                    || state.slotted.keys().filter(|k| k.author == author).count()
                        >= MAX_SLOTS_PER_AUTHOR)
            {
                return false;
            }
            let held = held && !slot.motion;
            let pending = state.slotted.entry(key).or_insert_with(|| Pending {
                payload: Arc::clone(&payload),
                motion: slot.motion,
                held,
                since: now,
                seq,
            });
            pending.payload = payload;
            pending.motion = slot.motion;
            pending.held = held;
            pending.seq = seq;
            let pending = state.slotted.get(&key);
            pending.map_or(now, |pending| due_at(&state.sent, &key, pending))
        } else {
            if state.events.len() >= MAX_PENDING_EVENTS {
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

    /// Take everything due at `now`, in posting order; with `ride`, also
    /// everything that may go early because something is being written to
    /// the connection anyway (see [`may_ride`]). When unslotted presence is
    /// taken, everything pending is taken with it, so it never overtakes
    /// presence posted before it.
    pub fn take(&self, now: Instant, ride: bool) -> Vec<DuePresence> {
        let mut state = self.state();
        state.armed = None;
        let everything = state
            .events
            .front()
            .is_some_and(|event| ride || later(event.since, BATCH_WINDOW) <= now);
        let keys: Vec<PresenceSlotKey> = state
            .slotted
            .iter()
            .filter(|(key, pending)| {
                everything
                    || due_at(&state.sent, key, pending) <= now
                    || (ride && may_ride(&state.sent, key, pending, now))
            })
            .map(|(key, _)| *key)
            .collect();
        let mut due: Vec<(u64, DuePresence)> = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(pending) = state.slotted.remove(&key) {
                if pending.motion {
                    // Pace on the schedule, not on when the writer got to
                    // it: a write that is late does not push every later
                    // move of a client that already sends at this pace
                    // later too. One that goes early restarts the pace.
                    let scheduled = due_at(&state.sent, &key, &pending).min(now);
                    state.sent.insert(key, scheduled);
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
        assert!(mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("caret 1"), t0, false));
        assert!(mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("caret 2"), t0, false));
        assert!(mailbox.post(ROOM, ALICE, Some(caret(2)), bytes("typing"), t0, false));
        assert!(mailbox.post(ROOM, BOB, Some(caret(1)), bytes("bob caret"), t0, false));
        assert!(
            mailbox.take(t0, false).is_empty(),
            "the batch window is open"
        );
        let due = mailbox.take(t0 + BATCH_WINDOW, false);
        assert_eq!(texts(&due), ["caret 2", "typing", "bob caret"]);
        assert!(due.iter().all(|presence| presence.key.is_some()));
        assert!(mailbox.take(t0 + BATCH_WINDOW, false).is_empty());
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
                false,
            );
            let next_post = posts
                .get(step + 1)
                .copied()
                .unwrap_or(t0 + Duration::from_secs(2));
            while let Some(due) = mailbox.next_due().filter(|due| *due < next_post) {
                sent.extend(texts(&mailbox.take(due, false)));
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
    fn a_late_write_does_not_hold_the_next_move_of_a_paced_client() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 1"), t0, false);
        assert_eq!(
            mailbox.next_due(),
            Some(t0),
            "a pointer waits for no company"
        );
        assert_eq!(texts(&mailbox.take(t0, false)), ["move 1"]);
        // A client paced to the gap: the next move arrives a gap later, is
        // due as it arrives, and the writer gets to it 10 ms late.
        let t1 = t0 + MOTION_GAP;
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 2"), t1, false);
        assert_eq!(mailbox.next_due(), Some(t1));
        let late = t1 + Duration::from_millis(10);
        assert_eq!(texts(&mailbox.take(late, false)), ["move 2"]);
        // The third move, again a gap later, is due as soon as it arrives:
        // the late write is not added to its wait.
        let t2 = t1 + MOTION_GAP;
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 3"), t2, false);
        assert_eq!(mailbox.next_due(), Some(t2));
    }

    #[test]
    fn unslotted_presence_keeps_its_order_and_brings_what_came_before() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        // A pointer that has just moved is held for its gap...
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 1"), t0, false);
        assert_eq!(texts(&mailbox.take(t0 + BATCH_WINDOW, false)), ["move 1"]);
        let t1 = t0 + Duration::from_millis(10);
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 2"), t1, false);
        // ...until an event follows it, which must not overtake it.
        mailbox.post(ROOM, ALICE, None, bytes("cleared"), t1, false);
        mailbox.post(ROOM, BOB, None, bytes("heartbeat"), t1, false);
        let due = mailbox.take(t1 + BATCH_WINDOW, false);
        assert_eq!(texts(&due), ["move 2", "cleared", "heartbeat"]);
        assert!(due[1].key.is_none());
    }

    #[test]
    fn a_full_mailbox_refuses_new_presence_but_still_updates_its_slots() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        let per_author = u64::try_from(MAX_SLOTS_PER_AUTHOR).unwrap();
        // One author is held to its share...
        for slot in 0..per_author {
            assert!(mailbox.post(ROOM, ALICE, Some(caret(slot)), bytes("x"), t0, false));
        }
        assert!(!mailbox.post(ROOM, ALICE, Some(caret(per_author)), bytes("x"), t0, false));
        assert!(mailbox.post(ROOM, ALICE, Some(caret(0)), bytes("newer"), t0, false));
        // ...and everybody together to the connection's.
        let authors = MAX_PENDING / MAX_SLOTS_PER_AUTHOR;
        for author in 1..authors {
            let author = [u8::try_from(author).unwrap().saturating_add(10); 32];
            for slot in 0..per_author {
                assert!(mailbox.post(ROOM, author, Some(caret(slot)), bytes("x"), t0, false));
            }
        }
        assert!(!mailbox.post(ROOM, BOB, Some(caret(0)), bytes("x"), t0, false));
        for _ in 0..MAX_PENDING_EVENTS {
            assert!(mailbox.post(ROOM, BOB, None, bytes("event"), t0, false));
        }
        assert!(!mailbox.post(ROOM, BOB, None, bytes("event"), t0, false));
        let due = mailbox.take(t0 + BATCH_WINDOW, false);
        assert_eq!(due.len(), MAX_PENDING + MAX_PENDING_EVENTS);
    }

    #[test]
    fn large_slotted_presence_is_delivered_as_an_event() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        let large: Arc<[u8]> = Arc::from(vec![7; MAX_SLOTTED_PRESENCE_BYTES + 1]);
        mailbox.post(ROOM, ALICE, Some(caret(1)), large.clone(), t0, false);
        mailbox.post(ROOM, ALICE, Some(caret(1)), large, t0, false);
        let due = mailbox.take(t0 + BATCH_WINDOW, false);
        assert_eq!(due.len(), 2, "neither copy is held by slot");
        assert!(due.iter().all(|presence| presence.key.is_none()));
    }

    #[test]
    fn a_held_caret_waits_for_its_delta_up_to_the_hold() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        assert!(mailbox.post(ROOM, ALICE, Some(caret(2)), bytes("typing"), t0, true));
        assert!(mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("caret"), t0, true));
        assert_eq!(mailbox.next_due(), Some(t0 + DELTA_HOLD));
        assert!(
            mailbox.take(t0 + BATCH_WINDOW, false).is_empty(),
            "held past the batch window"
        );
        // The delta is written: the held presence goes with it.
        let t1 = t0 + Duration::from_millis(7);
        assert_eq!(texts(&mailbox.take(t1, true)), ["typing", "caret"]);
        // A delta that never comes: the hold ends on its own.
        assert!(mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("later"), t1, true));
        assert!(
            mailbox
                .take(t1 + DELTA_HOLD - Duration::from_millis(1), false)
                .is_empty()
        );
        assert_eq!(texts(&mailbox.take(t1 + DELTA_HOLD, false)), ["later"]);
    }

    #[test]
    fn a_pointer_is_never_held() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move"), t0, true);
        // Neither for a delta nor for the batch window: it is due at once.
        assert_eq!(mailbox.next_due(), Some(t0));
    }

    #[test]
    fn a_newer_unheld_caret_is_due_after_the_batch_window() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("held"), t0, true);
        mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("moved"), t0, false);
        assert_eq!(texts(&mailbox.take(t0 + BATCH_WINDOW, false)), ["moved"]);
    }

    #[test]
    fn riding_takes_change_slots_before_their_window() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("caret"), t0, false);
        mailbox.post(ROOM, BOB, Some(caret(1)), bytes("bob caret"), t0, false);
        assert!(mailbox.take(t0, false).is_empty());
        assert_eq!(texts(&mailbox.take(t0, true)), ["caret", "bob caret"]);
        assert_eq!(mailbox.next_due(), None);
    }

    #[test]
    fn a_pointer_rides_only_after_half_its_gap() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        // A first move rides at once: nothing paces it yet.
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 1"), t0, false);
        assert_eq!(texts(&mailbox.take(t0, true)), ["move 1"]);
        let t1 = t0 + RIDE_GAP - Duration::from_millis(1);
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 2"), t1, false);
        assert!(
            mailbox.take(t1, true).is_empty(),
            "a pointer that went under the ride gap ago stays paced"
        );
        let t2 = t0 + RIDE_GAP;
        assert_eq!(texts(&mailbox.take(t2, true)), ["move 2"]);
        // Riding restarts its pace: the next move is due a gap later.
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 3"), t2, false);
        assert_eq!(mailbox.next_due(), Some(t2 + MOTION_GAP));
    }

    #[test]
    fn a_riding_event_brings_everything_posted_before_it() {
        let mailbox = PresenceMailbox::default();
        let t0 = Instant::now();
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 1"), t0, false);
        assert_eq!(texts(&mailbox.take(t0 + BATCH_WINDOW, false)), ["move 1"]);
        let t1 = t0 + Duration::from_millis(10);
        // Paced, and too recent to ride on its own...
        mailbox.post(ROOM, ALICE, Some(pointer(1)), bytes("move 2"), t1, false);
        assert!(mailbox.take(t1, true).is_empty());
        // ...but an event after it must not overtake it.
        mailbox.post(ROOM, ALICE, None, bytes("cleared"), t1, false);
        assert_eq!(texts(&mailbox.take(t1, true)), ["move 2", "cleared"]);
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
        mailbox.post(ROOM, ALICE, Some(caret(1)), bytes("caret"), posted, false);
        let woke = waiter.await.unwrap();
        assert!(woke >= posted + BATCH_WINDOW);
        assert_eq!(texts(&mailbox.take(woke, false)), ["caret"]);
    }
}
