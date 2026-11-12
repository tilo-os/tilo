//! Transport-neutral payloads exchanged between collaboration brokers.
//!
//! App-to-broker RPC uses the shared postcard wire, while broker-to-broker
//! replication uses the peer transport.  These types are the semantic hand-off
//! between them: peer messages no longer embed a serialized legacy UI request.

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
    pub fn to_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(self)
    }

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
    /// `idle`), the voice channel they are in, and whether their camera and
    /// screen are on there. Desktop presence: shells show it, apps never
    /// receive it.
    ///
    /// `camera` and `screen` end the message: adding them changed what a
    /// device reads after `call`, so the interface's version was raised with
    /// them.
    Availability {
        state: String,
        call: Option<String>,
        camera: bool,
        screen: bool,
    },
    /// What the sender says beside their pointer (cursor chat) in a room
    /// with no live plane: `text` while the bubble is open (empty with
    /// nothing typed yet), `None` once it closed; `sent` once they pressed
    /// Enter. Desktop presence: shells show it, apps never receive it.
    Chat {
        text: Option<String>,
        sent: bool,
    },
    /// The sender reacted with `emoji` from their pointer, in a room with no
    /// live plane; a new `seq` is a new reaction. Desktop presence.
    Reaction {
        seq: u32,
        emoji: String,
    },
    /// The sender waved at `target`, another member's live publisher; a new
    /// `seq` is a new wave, and restating the same one is the same wave.
    /// Desktop presence: a greeting, never an instruction.
    ///
    /// Appended so new decoders continue to read every older postcard frame;
    /// older peers reject only this greeting.
    Wave {
        seq: u32,
        target: String,
    },
    /// The sender is composing a message under the presence key (a channel, a direct
    /// conversation): a draft under way, never what it says. `false` says they stopped.
    /// Ephemeral like every other presence frame.
    ///
    /// Appended so new decoders continue to read every older postcard frame;
    /// older peers reject only this cue.
    Composing {
        composing: bool,
    },
    /// Who the sender's desktop shell shows itself as and what its screen
    /// looks like now: its windows, the one it works in, and the caret
    /// there. Desktop presence: shells draw the sender's face and a mini view
    /// of its screen, apps never receive it. The sender's pointer is not
    /// here; it travels as a tile-space pointer like every device's.
    ///
    /// Appended so new decoders continue to read every older postcard frame;
    /// older peers reject only what they cannot draw of a shell's screen.
    Shell(Box<crate::presence::ShellPresence>),
    /// The person's name, as they call themselves here: what everyone here
    /// calls them, whichever of their devices says it. A device names itself
    /// in its certificate; this names the person behind it, so a face, a row
    /// and a card name a person and never a machine. Desktop presence: shells
    /// show it, apps never receive it.
    ///
    /// Appended so new decoders continue to read every older postcard frame;
    /// older peers reject only the name, and go on showing the device's.
    Person {
        name: String,
    },
    /// When the person last changed the profile their workspaces keep for them (their name and
    /// picture), Unix milliseconds, as this device knows it: a reader fetches the profile again when
    /// it moves. Desktop presence: shells show the picture, apps never receive it.
    ///
    /// Appended so new decoders continue to read every older postcard frame;
    /// older peers reject only the stamp, and go on showing the picture they have.
    Profile {
        changed_ms: u64,
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
                camera: true,
                screen: false,
            },
            PeerPresencePayload::Chat {
                text: Some("look at the second paragraph".into()),
                sent: true,
            },
            PeerPresencePayload::Chat {
                text: None,
                sent: false,
            },
            PeerPresencePayload::Reaction {
                seq: 3,
                emoji: "\u{1f389}".into(),
            },
            PeerPresencePayload::Wave {
                seq: 4,
                target: "peer@8".into(),
            },
            PeerPresencePayload::Composing { composing: true },
            PeerPresencePayload::Composing { composing: false },
            PeerPresencePayload::Profile {
                changed_ms: 1_700_000_000_000,
            },
            PeerPresencePayload::Shell(Box::new(crate::presence::ShellPresence {
                seq: 7,
                sent_at_ms: 1_700_000_000_000,
                label: "Ada".into(),
                agent: false,
                colour: "#44aaff".into(),
                activity: None,
                place: Some("2".into()),
                size: [1440, 852],
                windows: vec![crate::presence::ShellWindow {
                    app: "so.tilo.notes".into(),
                    record: [7; 8],
                    rect: [0, 0, 10000, 10000],
                    focused: true,
                }],
                selection: Some(crate::presence::ShellSelection {
                    anchor: "0102".into(),
                    head: "0304".into(),
                }),
                chat: Some(crate::presence::ShellChat {
                    text: "here".into(),
                    sent: true,
                }),
                reaction: Some(crate::presence::ShellReaction {
                    seq: 3,
                    emoji: "🎉".into(),
                }),
            })),
            PeerPresencePayload::Person { name: "Ada".into() },
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
