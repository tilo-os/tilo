//! Which presence messages a relay may coalesce, and under what identity.
//!
//! Presence that states *where something is now* — a pointer, a caret, a
//! selection, the range being typed — is superseded by its next value: a
//! receiver draws only the latest one. A relay that holds such a message for
//! a few milliseconds may therefore replace it with a newer one from the same
//! author under the same [`PresenceSlot`], and forward only the latest. Every
//! other message (heartbeats, `SyncReq`, identity, arrivals and departures,
//! status) is an event and has no slot: it is delivered as sent.
//!
//! The relay reads the slot from the message it forwards, decoding only the
//! two variants that can have one ([`NeighbourMsg::SLOTTED_PRESENCE_TAGS`]),
//! and forwards the bytes unchanged. A slot only ever supersedes the same
//! authenticated author's presence, so a sender can cost itself its own
//! updates and nobody else's.

use crate::NeighbourMsg;
use collab_types::PeerPresencePayload;

/// The coalescing identity of a latest-state presence message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PresenceSlot {
    /// Same id from the same author: the newer message supersedes the older.
    pub id: u64,
    /// A continuously moving pointer, which a relay may forward at a lower
    /// rate than it is published (receivers glide between samples); a
    /// `false` slot (caret, selection, typing) is forwarded on every change.
    pub motion: bool,
}

impl PresenceSlot {
    fn of(kind: &str, motion: bool, parts: &[&[u8]]) -> Self {
        let mut hasher = blake3::Hasher::new_derive_key("tilo neighbour presence slot v1");
        hasher.update(kind.as_bytes());
        for part in parts {
            // Length-prefixed, so ("ab", "c") and ("a", "bc") differ.
            hasher.update(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
            hasher.update(part);
        }
        let digest = hasher.finalize();
        let mut id = [0; 8];
        id.copy_from_slice(digest.as_bytes().get(..8).unwrap_or(&[0; 8]));
        Self {
            id: u64::from_le_bytes(id),
            motion,
        }
    }
}

impl NeighbourMsg {
    /// Postcard variant indices of the only messages that can have a
    /// [`PresenceSlot`]: [`Self::PresenceMoved`] and [`Self::AppPresenceV2`].
    /// A reader that sees any other first byte need not decode the message
    /// (it may be a video frame) to know it has none. Wire-frozen, like
    /// [`Self::APP_DOCUMENT_TAG`]; a unit test pins both.
    pub const SLOTTED_PRESENCE_TAGS: [u8; 2] = [2, 13];

    /// The slot under which a relay may coalesce this message with a later
    /// one from the same author, or `None` when every copy must be delivered.
    pub fn presence_slot(&self) -> Option<PresenceSlot> {
        match self {
            Self::PresenceMoved { key, .. } => {
                Some(PresenceSlot::of("pointer", true, &[key.as_bytes()]))
            }
            Self::AppPresenceV2 {
                app_id,
                cluster_id,
                presence,
                ..
            } => {
                let (kind, motion) = match presence.payload {
                    PeerPresencePayload::Pointer { .. } => ("app-pointer", true),
                    PeerPresencePayload::Caret { .. } => ("app-caret", false),
                    PeerPresencePayload::Selection { .. } => ("app-selection", false),
                    PeerPresencePayload::Typing { .. } => ("app-typing", false),
                    // Arrivals, departures, following, interactions and
                    // status are events: each one is delivered.
                    _ => return None,
                };
                Some(PresenceSlot::of(
                    kind,
                    motion,
                    &[app_id.as_bytes(), cluster_id.as_bytes(), &presence.key],
                ))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::PeerPresence;

    fn app(key: &[u8], payload: PeerPresencePayload) -> NeighbourMsg {
        NeighbourMsg::AppPresenceV2 {
            peer: 7,
            app_id: "so.tilo.word-processor".into(),
            cluster_id: "room".into(),
            presence: PeerPresence {
                key: key.to_vec(),
                payload,
            },
            colour: None,
        }
    }

    fn pointer(key: &str, x: f64) -> NeighbourMsg {
        NeighbourMsg::PresenceMoved {
            peer: 7,
            user: "user".into(),
            key: key.into(),
            point: (x, 1.0),
            ts: 1,
            colour: None,
        }
    }

    #[test]
    fn latest_state_presence_has_a_slot_that_ignores_the_value() {
        let a = pointer("tile", 1.0).presence_slot().unwrap();
        assert!(a.motion);
        assert_eq!(Some(a), pointer("tile", 2.0).presence_slot());
        assert_ne!(Some(a), pointer("other tile", 2.0).presence_slot());

        let caret = app(b"field", PeerPresencePayload::Caret { pos: 1 })
            .presence_slot()
            .unwrap();
        assert!(!caret.motion);
        assert_eq!(
            Some(caret),
            app(b"field", PeerPresencePayload::Caret { pos: 9 }).presence_slot()
        );
        let typing = app(b"field", PeerPresencePayload::Typing { start: 1, end: 2 })
            .presence_slot()
            .unwrap();
        assert_ne!(caret, typing);
        assert!(
            app(b"field", PeerPresencePayload::Pointer { x: 1.0, y: 2.0 })
                .presence_slot()
                .unwrap()
                .motion
        );
    }

    #[test]
    fn only_messages_with_a_slotted_tag_can_have_a_slot() {
        let first = |message: &NeighbourMsg| postcard::to_allocvec(message).unwrap()[0];
        assert_eq!(
            [
                first(&pointer("tile", 1.0)),
                first(&app(b"field", PeerPresencePayload::Caret { pos: 1 })),
            ],
            NeighbourMsg::SLOTTED_PRESENCE_TAGS
        );
        let cleared = NeighbourMsg::PresenceCleared {
            peer: 7,
            key: "tile".into(),
        };
        assert!(!NeighbourMsg::SLOTTED_PRESENCE_TAGS.contains(&first(&cleared)));
    }

    #[test]
    fn events_have_no_slot() {
        assert_eq!(
            app(b"tab", PeerPresencePayload::Arrived).presence_slot(),
            None
        );
        assert_eq!(app(b"tab", PeerPresencePayload::Left).presence_slot(), None);
        assert_eq!(
            NeighbourMsg::PresenceCleared {
                peer: 7,
                key: "tile".into()
            }
            .presence_slot(),
            None
        );
    }
}
