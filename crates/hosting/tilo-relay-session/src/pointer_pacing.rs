//! Pointer presence, paced to the rate the relay forwards it, on every path.
//!
//! The shell and the toolkit publish a moving pointer at about 30 Hz, and the
//! relay forwards each pointer slot to every other member at most every
//! [`POINTER_GAP`] (20 Hz). Receivers glide over the gap between two samples,
//! so a third of those moves buy nothing a receiver can show: on the relay
//! path they are coalesced away on arrival, after costing an upstream packet
//! and, on a short path, an acknowledgement-only packet back; on the direct
//! (gossip) path they cost every neighbour a packet and an acknowledgement.
//!
//! Both paths therefore hold pointer presence per slot, with one
//! [`PointerPacer`] and one classification ([`slot_pacing`]): the session
//! writer for the relay, the shell's network actor for gossip.
//!
//! - **Leading edge.** A move of a slot that has not been written within the
//!   gap is written at once.
//! - **Latest wins, when the gap ends.** A move inside the gap replaces the
//!   held one and is written when the gap ends, so the final resting position
//!   always goes.
//! - **Fresh rather than stale.** A 30 Hz producer paced to 20 Hz leaves the
//!   held move up to a whole producer period old when the gap ends. When the
//!   producer's next move is due within [`FRESHER_WAIT`], and sooner than the
//!   held one has gone stale, the writer waits for it and writes that one at
//!   once instead; if it does not come, the held move goes. A receiver so
//!   gets moves that are on average a few milliseconds old, not half a
//!   producer period, and a resting position is at most [`FRESHER_WAIT`] late.
//! - **Order where it matters.** Anything but a delta or other latest-state
//!   presence (a caret, a selection, a typing range) takes every held pointer
//!   ahead of it, in the same write: an app's `Left` never overtakes its last
//!   pointer, and nothing is written for a room after its `Unsubscribe`.
//!   Deltas and carets may pass a held pointer, as the relay lets them.
//!
//! Only slots the sender itself declares as motion
//! ([`neighbour_wire::PresenceSlot::motion`]) are paced; the author is never
//! looked at, so a person's and an agent's pointer travel alike.

use std::collections::HashMap;
use std::time::Duration;

use n0_future::time::Instant;

pub use crate::coedit::POINTER_GAP;
use crate::coedit::{AttributedPresence, presence_slot};
use crate::transport::Envelope;

/// One pointer slot of one room: `(workspace_id, slot id)`.
pub type PointerKey = (u64, u64);

/// How an outbound message is paced.
#[derive(Debug, PartialEq, Eq)]
pub enum Pacing {
    /// A pointer move: held in its slot.
    Pointer(PointerKey),
    /// A delta or latest-state presence other than a pointer: written now,
    /// and may pass held pointers.
    Passes,
    /// Everything else: written now, after every held pointer.
    Ordered,
}

/// How presence with `slot` in room `workspace_id` is paced: a motion slot is
/// held, any other slot passes, and a message without one is an event.
#[must_use]
pub fn slot_pacing(workspace_id: u64, slot: Option<neighbour_wire::PresenceSlot>) -> Pacing {
    match slot {
        Some(slot) if slot.motion => Pacing::Pointer((workspace_id, slot.id)),
        Some(_) => Pacing::Passes,
        None => Pacing::Ordered,
    }
}

/// How the session writer paces `envelope`.
pub(crate) fn pacing(envelope: &Envelope) -> Pacing {
    match envelope {
        Envelope::Delta { .. } => Pacing::Passes,
        Envelope::Presence {
            workspace_id,
            payload,
        } => slot_pacing(
            *workspace_id,
            AttributedPresence::decode(payload).and_then(|presence| presence_slot(&presence.body)),
        ),
        _ => Pacing::Ordered,
    }
}

/// The longest a held move that has gone stale waits past the end of its gap
/// for a fresher one its producer is about to publish.
pub const FRESHER_WAIT: Duration = Duration::from_millis(15);
/// How late a producer's next move may be against its own rhythm and still
/// be waited for.
pub const ARRIVAL_SLACK: Duration = Duration::from_millis(3);

/// A move held back, with its posting order and when it was posted.
struct Held<T> {
    order: u64,
    item: T,
    posted: Instant,
}

struct Slot<T> {
    /// When this slot was last written.
    sent: Option<Instant>,
    /// The newest move not yet written.
    held: Option<Held<T>>,
    /// When the newest move was posted, and how long after the one before
    /// it: the producer's rhythm while it moves faster than the gap.
    posted: Option<Instant>,
    rhythm: Option<Duration>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            sent: None,
            held: None,
            posted: None,
            rhythm: None,
        }
    }
}

impl<T> Slot<T> {
    /// When the held move falls due, if one is held.
    ///
    /// At the end of its gap, unless the producer's next move is due within
    /// [`FRESHER_WAIT`] and sooner than the held move has gone stale: then
    /// that fresher move is waited for, and the held one goes only if it does
    /// not come. A move posted after its gap ended is fresh and goes at once.
    fn due(&self) -> Option<Instant> {
        let held = self.held.as_ref()?;
        let Some(gap_end) = self.sent.map(|sent| later(sent, POINTER_GAP)) else {
            return Some(held.posted);
        };
        let staleness = gap_end.saturating_duration_since(held.posted);
        let fresher = self
            .rhythm
            .map(|rhythm| later(held.posted, rhythm))
            .filter(|next| {
                let wait = next.saturating_duration_since(gap_end);
                *next > gap_end && wait <= FRESHER_WAIT && wait < staleness
            });
        Some(fresher.map_or(gap_end, |next| later(next, ARRIVAL_SLACK)))
    }

    /// Whether this slot paces nothing any more: nothing held, and its gap
    /// over, so a move after this pause goes at once anyway.
    fn idle(&self, now: Instant) -> bool {
        self.held.is_none() && self.sent.is_none_or(|sent| now >= later(sent, POINTER_GAP))
    }
}

/// `at + by`, or `at` itself past the end of time.
fn later(at: Instant, by: Duration) -> Instant {
    at.checked_add(by).unwrap_or(at)
}

/// The pointer moves a writer holds back, per slot: envelopes in the relay
/// session writer, encoded gossip messages in the shell's network actor.
pub struct PointerPacer<T> {
    slots: HashMap<PointerKey, Slot<T>>,
    posted: u64,
}

impl<T> Default for PointerPacer<T> {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            posted: 0,
        }
    }
}

/// What the writer does with one outbound state envelope.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admitted {
    /// A pointer move, held in its slot until [`PointerPacer::take_due`].
    Held,
    /// Write `ahead` and then `envelope`, now.
    Write {
        ahead: Vec<Envelope>,
        envelope: Envelope,
    },
}

impl PointerPacer<Envelope> {
    /// Admit one outbound state envelope at `now`: hold a pointer move, or
    /// say what to write, with the held pointers it must not overtake.
    pub(crate) fn admit(&mut self, envelope: Envelope, now: Instant) -> Admitted {
        let ahead = match pacing(&envelope) {
            Pacing::Pointer(key) => {
                self.hold(key, envelope, now);
                return Admitted::Held;
            }
            Pacing::Passes => Vec::new(),
            Pacing::Ordered => self.take_all(now),
        };
        Admitted::Write { ahead, envelope }
    }
}

impl<T> PointerPacer<T> {
    /// Hold `item`, posted at `now`, as the newest move of `key`, replacing
    /// an older one.
    pub fn hold(&mut self, key: PointerKey, item: T, now: Instant) {
        let order = self.posted;
        self.posted = self.posted.wrapping_add(1);
        let slot = self.slots.entry(key).or_default();
        slot.rhythm = slot
            .posted
            .map(|before| now.saturating_duration_since(before))
            .filter(|rhythm| !rhythm.is_zero() && *rhythm <= POINTER_GAP);
        slot.posted = Some(now);
        slot.held = Some(Held {
            order,
            item,
            posted: now,
        });
    }

    /// When the next held move falls due, if any is held.
    #[must_use]
    pub fn next_due(&self) -> Option<Instant> {
        self.slots.values().filter_map(Slot::due).min()
    }

    /// Take every held move that is due at `now`, in posting order, and mark
    /// those slots written. Idle slots are forgotten.
    pub fn take_due(&mut self, now: Instant) -> Vec<T> {
        let mut due = Vec::new();
        self.slots.retain(|_, slot| {
            if slot.due().is_some_and(|at| at <= now) {
                due.extend(slot.held.take());
                slot.sent = Some(now);
            }
            !slot.idle(now)
        });
        in_order(due)
    }

    /// Take every held move, due or not, in posting order, and mark those
    /// slots written: what goes ahead of a message that must not overtake
    /// them.
    pub fn take_all(&mut self, now: Instant) -> Vec<T> {
        let mut all = Vec::new();
        for slot in self.slots.values_mut() {
            if let Some(held) = slot.held.take() {
                slot.sent = Some(now);
                all.push(held);
            }
        }
        in_order(all)
    }
}

fn in_order<T>(mut moves: Vec<Held<T>>) -> Vec<T> {
    moves.sort_by_key(|held| held.order);
    moves.into_iter().map(|held| held.item).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use neighbour_wire::NeighbourMsg;
    use std::time::Duration;

    const ROOM: u64 = 7;

    fn presence(message: &NeighbourMsg) -> Envelope {
        Envelope::Presence {
            workspace_id: ROOM,
            payload: AttributedPresence {
                author: [1; 32],
                body: postcard::to_allocvec(message).unwrap(),
            }
            .encode()
            .unwrap(),
        }
    }

    pub(crate) fn pointer(key: &str, x: f64) -> Envelope {
        presence(&NeighbourMsg::PresenceMoved {
            peer: 3,
            user: "someone".into(),
            key: key.into(),
            point: (x, 0.0),
            ts: 0,
            colour: None,
        })
    }

    fn app(payload: collab_types::PeerPresencePayload) -> Envelope {
        presence(&NeighbourMsg::AppPresenceV2 {
            peer: 3,
            app_id: "so.tilo.word-processor".into(),
            cluster_id: "room".into(),
            presence: collab_types::PeerPresence {
                key: b"field".to_vec(),
                payload,
            },
            colour: None,
        })
    }

    /// An app's caret: latest-state presence that is not paced.
    pub(crate) fn caret(pos: u32) -> Envelope {
        app(collab_types::PeerPresencePayload::Caret { pos: pos.into() })
    }

    /// An app's pointer leaving its window: an event.
    pub(crate) fn left() -> Envelope {
        app(collab_types::PeerPresencePayload::Left)
    }

    fn key_of(envelope: &Envelope) -> PointerKey {
        match pacing(envelope) {
            Pacing::Pointer(key) => key,
            other => panic!("not a pointer: {other:?}"),
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn only_motion_slots_are_paced_and_events_keep_their_order() {
        use collab_types::PeerPresencePayload as P;

        assert!(matches!(pacing(&pointer("tile", 1.0)), Pacing::Pointer(_)));
        assert!(matches!(
            pacing(&app(P::Pointer { x: 1.0, y: 2.0 })),
            Pacing::Pointer(_)
        ));
        assert_eq!(pacing(&app(P::Caret { pos: 4 })), Pacing::Passes);
        assert_eq!(pacing(&app(P::Typing { start: 1, end: 2 })), Pacing::Passes);
        assert_eq!(pacing(&app(P::Left)), Pacing::Ordered);
        assert_eq!(pacing(&Envelope::Ping), Pacing::Ordered);
        assert_eq!(
            pacing(&Envelope::Unsubscribe { workspace_id: ROOM }),
            Pacing::Ordered
        );
        // A body that is not attributed co-edit presence is an event.
        assert_eq!(
            pacing(&Envelope::Presence {
                workspace_id: ROOM,
                payload: vec![0xff],
            }),
            Pacing::Ordered
        );
    }

    #[test]
    fn a_move_after_a_pause_goes_at_once() {
        let t0 = Instant::now();
        let mut pacer = PointerPacer::default();
        let first = pointer("tile", 1.0);
        pacer.hold(key_of(&first), first.clone(), t0);
        assert_eq!(pacer.take_due(t0), vec![first]);
        assert_eq!(pacer.next_due(), None);
        // A whole gap later the slot is idle again: the next move goes at once.
        let t1 = t0 + POINTER_GAP;
        let later = pointer("tile", 2.0);
        pacer.hold(key_of(&later), later.clone(), t1);
        assert_eq!(pacer.take_due(t1), vec![later]);
    }

    #[test]
    fn a_30_hz_stream_goes_at_20_hz_and_its_last_move_soon_after_the_gaps_end() {
        let t0 = Instant::now();
        let mut pacer = PointerPacer::default();
        let mut written = Vec::new();
        for step in 0..10_u32 {
            let now = t0 + ms(33) * step;
            // The writer wakes for every move and every due instant.
            while let Some(due) = pacer.next_due().filter(|due| *due < now) {
                written.extend(pacer.take_due(due).into_iter().map(|e| (due, e)));
            }
            let envelope = pointer("tile", f64::from(step));
            pacer.hold(key_of(&envelope), envelope, now);
            written.extend(pacer.take_due(now).into_iter().map(|e| (now, e)));
        }
        let last_posted = t0 + ms(33 * 9);
        let due = pacer.next_due().expect("the last move is held");
        assert!(due <= last_posted + POINTER_GAP);
        written.extend(pacer.take_due(due).into_iter().map(|e| (due, e)));

        for pair in written.windows(2) {
            assert!(
                pair[1].0.duration_since(pair[0].0) >= POINTER_GAP,
                "two writes of one slot closer than the gap"
            );
        }
        assert_eq!(written.first().map(|(_, e)| e), Some(&pointer("tile", 0.0)));
        assert_eq!(
            written.last().map(|(_, e)| e),
            Some(&pointer("tile", 9.0)),
            "the resting position must be written"
        );
        // 297 ms of moves: the first at once, then about one per gap.
        assert_eq!(written.len(), 7, "{written:?}");
        assert!(pacer.next_due().is_none());
    }

    #[test]
    fn a_stale_held_move_waits_briefly_for_a_fresher_one() {
        let t0 = Instant::now();
        let mut pacer = PointerPacer::default();
        let a = pointer("tile", 1.0);
        pacer.hold(key_of(&a), a.clone(), t0);
        assert_eq!(pacer.take_due(t0), vec![a.clone()]);
        // Posted 32 ms in, this move is 18 ms old when the gap ends; the
        // producer's next is due 14 ms after that, so it is waited for.
        let b = pointer("tile", 2.0);
        pacer.hold(key_of(&b), b.clone(), t0 + ms(32));
        let fresher_due = t0 + ms(64) + ARRIVAL_SLACK;
        assert_eq!(pacer.next_due(), Some(fresher_due));
        assert!(pacer.take_due(t0 + POINTER_GAP).is_empty());
        // It comes, and goes at once in place of the stale one.
        let c = pointer("tile", 3.0);
        pacer.hold(key_of(&c), c.clone(), t0 + ms(64));
        assert_eq!(pacer.take_due(t0 + ms(64)), vec![c]);

        // If the producer stops instead, the held move still goes.
        let mut pacer = PointerPacer::default();
        pacer.hold(key_of(&a), a.clone(), t0);
        assert_eq!(pacer.take_due(t0), vec![a]);
        pacer.hold(key_of(&b), b.clone(), t0 + ms(32));
        assert_eq!(pacer.take_due(fresher_due), vec![b]);
        assert!(fresher_due <= t0 + POINTER_GAP + FRESHER_WAIT + ARRIVAL_SLACK);
    }

    #[test]
    fn a_held_move_that_is_still_fresh_goes_when_the_gap_ends() {
        let t0 = Instant::now();
        let mut pacer = PointerPacer::default();
        let a = pointer("tile", 1.0);
        pacer.hold(key_of(&a), a.clone(), t0);
        assert_eq!(pacer.take_due(t0), vec![a]);
        // Posted 40 ms in, 10 ms old at the gap's end: waiting the 30 ms for
        // the producer's next move would cost more than it saves.
        let b = pointer("tile", 2.0);
        pacer.hold(key_of(&b), b.clone(), t0 + ms(40));
        assert_eq!(pacer.next_due(), Some(t0 + POINTER_GAP));
        assert_eq!(pacer.take_due(t0 + POINTER_GAP), vec![b]);
    }

    #[test]
    fn slots_are_paced_apart_and_taken_in_posting_order() {
        let t0 = Instant::now();
        let mut pacer = PointerPacer::default();
        let a = pointer("tile a", 1.0);
        let b = pointer("tile b", 1.0);
        pacer.hold(key_of(&a), a.clone(), t0);
        assert_eq!(pacer.take_due(t0), vec![a]);
        pacer.hold(key_of(&b), b.clone(), t0 + ms(10));
        assert_eq!(pacer.take_due(t0 + ms(10)), vec![b]);

        let a2 = pointer("tile a", 2.0);
        let b2 = pointer("tile b", 2.0);
        pacer.hold(key_of(&b2), b2.clone(), t0 + ms(20));
        pacer.hold(key_of(&a2), a2.clone(), t0 + ms(20));
        assert!(pacer.take_due(t0 + ms(20)).is_empty());
        assert_eq!(pacer.next_due(), Some(t0 + POINTER_GAP));
        assert_eq!(pacer.take_all(t0 + ms(20)), vec![b2, a2]);
        assert!(pacer.next_due().is_none());
    }

    #[test]
    fn idle_slots_are_forgotten() {
        let t0 = Instant::now();
        let mut pacer = PointerPacer::default();
        for slot in 0..100_u32 {
            let envelope = pointer(&format!("tile {slot}"), 0.0);
            pacer.hold(key_of(&envelope), envelope, t0);
        }
        assert_eq!(pacer.take_due(t0).len(), 100);
        assert_eq!(pacer.slots.len(), 100);
        assert!(pacer.take_due(t0 + POINTER_GAP).is_empty());
        assert!(pacer.slots.is_empty());
    }

    #[test]
    fn the_slot_alone_decides_and_any_item_can_be_held() {
        use collab_types::PeerPresencePayload as P;

        // The direct path classifies the message it is about to broadcast
        // with the same function the session writer uses.
        let moved = NeighbourMsg::PresenceMoved {
            peer: 3,
            user: "someone".into(),
            key: "tile".into(),
            point: (1.0, 0.0),
            ts: 0,
            colour: None,
        };
        let slot = moved.presence_slot();
        assert_eq!(
            slot_pacing(ROOM, slot),
            Pacing::Pointer((ROOM, slot.unwrap().id))
        );
        assert_eq!(
            slot_pacing(ROOM, slot),
            pacing(&pointer("tile", 1.0)),
            "both paths key a pointer alike"
        );
        let caret = NeighbourMsg::AppPresenceV2 {
            peer: 3,
            app_id: "so.tilo.word-processor".into(),
            cluster_id: "room".into(),
            presence: collab_types::PeerPresence {
                key: b"field".to_vec(),
                payload: P::Caret { pos: 1 },
            },
            colour: None,
        };
        assert_eq!(slot_pacing(ROOM, caret.presence_slot()), Pacing::Passes);
        assert_eq!(slot_pacing(ROOM, None), Pacing::Ordered);

        // Held items need not be envelopes: the gossip path holds encoded
        // messages, paced exactly alike.
        let t0 = Instant::now();
        let mut pacer = PointerPacer::<&str>::default();
        pacer.hold((ROOM, 1), "first", t0);
        assert_eq!(pacer.take_due(t0), vec!["first"]);
        pacer.hold((ROOM, 1), "second", t0 + ms(40));
        pacer.hold((ROOM, 1), "third", t0 + ms(45));
        assert!(pacer.take_due(t0 + ms(45)).is_empty());
        assert_eq!(pacer.next_due(), Some(t0 + POINTER_GAP));
        assert_eq!(pacer.take_due(t0 + POINTER_GAP), vec!["third"]);
    }
}
