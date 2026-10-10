//! A session's own pointer presence, paced to the rate the relay forwards it.
//!
//! The shell and the toolkit publish a moving pointer at about 30 Hz, and the
//! relay forwards each pointer slot to every other member at most every
//! [`POINTER_GAP`] (20 Hz). Without pacing here a third of the pointer packets
//! a client sends are coalesced away on arrival, after costing an upstream
//! packet and, on a short path, an acknowledgement-only packet back.
//!
//! The session writer therefore holds pointer presence per slot:
//!
//! - **Leading edge.** A move of a slot that has not been written within the
//!   gap is written at once.
//! - **Latest wins, never later than the gap.** A move inside the gap replaces
//!   the held one and is written when the gap ends, so the final resting
//!   position always goes, at most [`POINTER_GAP`] after the previous write.
//! - **Order where it matters.** Anything but a delta or other latest-state
//!   presence (a caret, a selection, a typing range) takes every held pointer
//!   ahead of it, in the same write: an app's `Left` never overtakes its last
//!   pointer, and nothing is written for a room after its `Unsubscribe`.
//!   Deltas and carets may pass a held pointer, as the relay lets them.
//!
//! Only slots the sender itself declares as motion
//! ([`tilo_peer_state::PresenceSlot::motion`]) are paced; the author is never
//! looked at, so a person's and an agent's pointer travel alike.

use std::collections::HashMap;

use n0_future::time::Instant;

use crate::coedit::{AttributedPresence, POINTER_GAP, presence_slot};
use crate::transport::Envelope;

/// One pointer slot of one room: `(workspace_id, slot id)`.
pub(crate) type PointerKey = (u64, u64);

/// How the session writer treats one outbound state envelope.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pacing {
    /// A pointer move: held in its slot.
    Pointer(PointerKey),
    /// A delta or latest-state presence other than a pointer: written now,
    /// and may pass held pointers.
    Passes,
    /// Everything else: written now, after every held pointer.
    Ordered,
}

/// How `envelope` is paced.
pub(crate) fn pacing(envelope: &Envelope) -> Pacing {
    match envelope {
        Envelope::Delta { .. } => Pacing::Passes,
        Envelope::Presence {
            workspace_id,
            payload,
        } => match AttributedPresence::decode(payload)
            .and_then(|presence| presence_slot(&presence.body))
        {
            Some(slot) if slot.motion => Pacing::Pointer((*workspace_id, slot.id)),
            Some(_) => Pacing::Passes,
            None => Pacing::Ordered,
        },
        _ => Pacing::Ordered,
    }
}

#[derive(Default)]
struct Slot {
    /// When this slot was last written.
    sent: Option<Instant>,
    /// The newest move not yet written, with its posting order.
    held: Option<(u64, Envelope)>,
}

impl Slot {
    fn ready(&self, now: Instant) -> bool {
        self.sent.is_none_or(|sent| now >= ready_at(sent))
    }
}

fn ready_at(sent: Instant) -> Instant {
    sent.checked_add(POINTER_GAP).unwrap_or(sent)
}

/// The pointer moves a session writer holds back, per slot.
#[derive(Default)]
pub(crate) struct PointerPacer {
    slots: HashMap<PointerKey, Slot>,
    posted: u64,
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

impl PointerPacer {
    /// Admit one outbound state envelope at `now`: hold a pointer move, or
    /// say what to write, with the held pointers it must not overtake.
    pub(crate) fn admit(&mut self, envelope: Envelope, now: Instant) -> Admitted {
        let ahead = match pacing(&envelope) {
            Pacing::Pointer(key) => {
                self.hold(key, envelope);
                return Admitted::Held;
            }
            Pacing::Passes => Vec::new(),
            Pacing::Ordered => self.take_all(now),
        };
        Admitted::Write { ahead, envelope }
    }

    /// Hold `envelope` as the newest move of `key`, replacing an older one.
    pub(crate) fn hold(&mut self, key: PointerKey, envelope: Envelope) {
        let order = self.posted;
        self.posted = self.posted.wrapping_add(1);
        self.slots.entry(key).or_default().held = Some((order, envelope));
    }

    /// When the next held move falls due, if any is held; `now` for one that
    /// is due already.
    pub(crate) fn next_due(&self, now: Instant) -> Option<Instant> {
        self.slots
            .values()
            .filter(|slot| slot.held.is_some())
            .map(|slot| slot.sent.map_or(now, ready_at))
            .min()
    }

    /// Take every held move whose slot may be written at `now`, in posting
    /// order, and mark those slots written. Slots idle for a whole gap are
    /// forgotten: a move after such a pause is written at once anyway.
    pub(crate) fn take_due(&mut self, now: Instant) -> Vec<Envelope> {
        let mut due = Vec::new();
        self.slots.retain(|_, slot| {
            if !slot.ready(now) {
                return true;
            }
            let Some(held) = slot.held.take() else {
                return false;
            };
            slot.sent = Some(now);
            due.push(held);
            true
        });
        in_order(due)
    }

    /// Take every held move, due or not, in posting order, and mark those
    /// slots written: what goes ahead of an envelope that must not overtake
    /// them.
    pub(crate) fn take_all(&mut self, now: Instant) -> Vec<Envelope> {
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

fn in_order(mut moves: Vec<(u64, Envelope)>) -> Vec<Envelope> {
    moves.sort_by_key(|(order, _)| *order);
    moves.into_iter().map(|(_, envelope)| envelope).collect()
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "the assertions add fixed durations to a fixed instant"
)]
pub(crate) mod tests {
    use super::*;
    use std::time::Duration;
    use tilo_peer_state::NeighbourMsg;

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
        pacer.hold(key_of(&first), first.clone());
        assert_eq!(pacer.take_due(t0), vec![first]);
        assert_eq!(pacer.next_due(t0), None);
        // A whole gap later the slot is idle again: the next move goes at once.
        let later = pointer("tile", 2.0);
        pacer.hold(key_of(&later), later.clone());
        assert_eq!(pacer.take_due(t0 + POINTER_GAP), vec![later]);
    }

    #[test]
    fn a_30_hz_stream_goes_at_20_hz_and_its_last_move_by_the_gaps_end() {
        let t0 = Instant::now();
        let mut pacer = PointerPacer::default();
        let mut written = Vec::new();
        for step in 0..10_u32 {
            let now = t0 + ms(33) * step;
            // The writer wakes for every move and every due instant.
            while let Some(due) = pacer.next_due(now).filter(|due| *due < now) {
                written.extend(pacer.take_due(due).into_iter().map(|e| (due, e)));
            }
            let envelope = pointer("tile", f64::from(step));
            pacer.hold(key_of(&envelope), envelope);
            written.extend(pacer.take_due(now).into_iter().map(|e| (now, e)));
        }
        let last_posted = t0 + ms(33 * 9);
        let due = pacer.next_due(last_posted).expect("the last move is held");
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
        // 297 ms of moves: the first at once, then one per gap.
        assert_eq!(written.len(), 7, "{written:?}");
        assert!(pacer.next_due(due).is_none());
    }

    #[test]
    fn slots_are_paced_apart_and_taken_in_posting_order() {
        let t0 = Instant::now();
        let mut pacer = PointerPacer::default();
        let a = pointer("tile a", 1.0);
        let b = pointer("tile b", 1.0);
        pacer.hold(key_of(&a), a.clone());
        assert_eq!(pacer.take_due(t0), vec![a]);
        pacer.hold(key_of(&b), b.clone());
        assert_eq!(pacer.take_due(t0 + ms(10)), vec![b]);

        let a2 = pointer("tile a", 2.0);
        let b2 = pointer("tile b", 2.0);
        pacer.hold(key_of(&b2), b2.clone());
        pacer.hold(key_of(&a2), a2.clone());
        assert!(pacer.take_due(t0 + ms(20)).is_empty());
        assert_eq!(pacer.next_due(t0 + ms(20)), Some(t0 + POINTER_GAP));
        assert_eq!(pacer.take_all(t0 + ms(20)), vec![b2, a2]);
        assert!(pacer.next_due(t0 + ms(20)).is_none());
    }

    #[test]
    fn idle_slots_are_forgotten() {
        let t0 = Instant::now();
        let mut pacer = PointerPacer::default();
        for slot in 0..100_u32 {
            let envelope = pointer(&format!("tile {slot}"), 0.0);
            pacer.hold(key_of(&envelope), envelope);
        }
        assert_eq!(pacer.take_due(t0).len(), 100);
        assert_eq!(pacer.slots.len(), 100);
        assert!(pacer.take_due(t0 + POINTER_GAP).is_empty());
        assert!(pacer.slots.is_empty());
    }
}
