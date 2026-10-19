//! Transport-neutral payloads exchanged between collaboration brokers.
//!
//! App-to-broker RPC uses Cap'n Proto, while broker-to-broker replication
//! uses the peer transport.  These types are the semantic hand-off between
//! them: peer messages no longer embed a serialized legacy UI request.

use serde::{Deserialize, Serialize};

use crate::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes};

/// One keyed presence update ready for broker-to-broker replication.
///
/// `key` is the canonical postcard representation of the app's presence key.
/// The broker does not interpret application-specific key variants, but it can
/// forward them without depending on a UI request enum.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerPresence {
    pub key: Vec<u8>,
    pub payload: PeerPresencePayload,
}

impl PeerPresence {
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if canonical serialisation fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }

    /// # Errors
    ///
    /// Returns [`CanonicalError`] if `bytes` fails to decode as `Self`.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CanonicalError> {
        from_canonical_bytes(bytes)
    }
}

/// Presence semantics shared by the typed local service and peer transport.
/// Character positions are fixed-width on the wire rather than architecture-
/// sized `usize`; conversion back into a local model is checked by the broker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PeerPresencePayload {
    Pointer {
        x: f32,
        y: f32,
    },
    Caret {
        pos: u64,
    },
    Selection {
        start: u64,
        end: u64,
    },
    Typing {
        start: u64,
        end: u64,
    },
    Arrived,
    Left,
    /// An ephemeral, room-scoped relationship cue: the authenticated sender
    /// is following `target`, or has stopped when it is `None`. This is not
    /// a collaboration command and must never be persisted or interpreted as
    /// permission to control another participant's view.
    ///
    /// Appended so new decoders continue to read every older postcard frame;
    /// older peers reject only this optional visual enhancement.
    Following {
        target: Option<String>,
    },
    /// Versioned semantic action lifecycle. The peer envelope supplies the
    /// authenticated actor; this JSON contains no identity or screen-space
    /// coordinates.
    InteractionJson(String),
    /// Short transient intention. Empty text clears the previous status.
    Status {
        text: String,
        automatic: bool,
    },
    /// How available the sender is (`available`, `focusing`, `away`,
    /// `idle`) and the voice channel they are in. Desktop presence: shells
    /// show it, apps never receive it.
    Availability {
        state: String,
        call: Option<String>,
    },
    /// The sender waves at `target`, a roster id. Desktop presence.
    Wave {
        target: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> Vec<PeerPresencePayload> {
        vec![
            PeerPresencePayload::Pointer { x: 12.5, y: -4.0 },
            PeerPresencePayload::Caret { pos: 42 },
            PeerPresencePayload::Selection { start: 3, end: 99 },
            PeerPresencePayload::Typing { start: 8, end: 13 },
            PeerPresencePayload::Arrived,
            PeerPresencePayload::Left,
            PeerPresencePayload::Following {
                target: Some("peer@7".into()),
            },
            PeerPresencePayload::Following { target: None },
            PeerPresencePayload::InteractionJson(
                r#"{"version":1,"invocation":"00000000-0000-0000-0000-000000000001"}"#.into(),
            ),
            PeerPresencePayload::Status {
                text: "Reviewing Q2 assumptions".into(),
                automatic: true,
            },
            PeerPresencePayload::Availability {
                state: "idle".into(),
                call: Some("voice".into()),
            },
            PeerPresencePayload::Wave {
                target: "peer@7".into(),
            },
        ]
    }

    /// Every presence variant round-trips deterministically; trailing or
    /// truncated frames are rejected.
    #[test]
    fn presence_frames_round_trip_exactly_and_reject_extra_or_missing_bytes() {
        for payload in fixtures() {
            let presence = PeerPresence {
                key: vec![0, 1, 2, 255],
                payload,
            };
            let first = presence.to_bytes().unwrap();
            let second = presence.to_bytes().unwrap();
            assert_eq!(first, second, "canonical encoding must be deterministic");
            assert_eq!(PeerPresence::from_bytes(&first).unwrap(), presence);
        }

        // Case: trailing and truncated peer frames are rejected.
        let frame = PeerPresence {
            key: vec![7],
            payload: PeerPresencePayload::Arrived,
        }
        .to_bytes()
        .unwrap();
        let mut trailing = frame.clone();
        trailing.push(0);
        assert!(PeerPresence::from_bytes(&trailing).is_err());
        assert!(PeerPresence::from_bytes(&frame[..frame.len() - 1]).is_err());
    }
}
