//! Voice calls in the rooms a relay governs.
//!
//! A call belongs to a room's governance: the room's governor records each
//! call, bound to one voice channel of a chat record, with the directions its
//! participants may use and per-person denials. A room's owner or an
//! administrator starts, ends and moderates calls with
//! [`Envelope::VoiceCall`](crate::Envelope::VoiceCall).
//!
//! A device subscribed to the room joins an active call with
//! [`Envelope::VoiceJoin`](crate::Envelope::VoiceJoin) and gets a
//! [`VoiceGrant`]: an opaque token for the admission preface of the relay's
//! `moq-lite` voice carrier, the exact namespace it publishes and those of the
//! other participants it hears. It repeats the join with its participation
//! every [`HEARTBEAT`]; a participation not heard from for
//! [`PARTICIPATION_TTL`], or whose connection closed, ends. When the roster
//! changes, the relay sends each remaining participant a fresh grant.
//!
//! A device that does not read the room's governance itself (a browser
//! session) learns its calls with
//! [`Envelope::VoiceCalls`](crate::Envelope::VoiceCalls).
//!
//! Browser and native devices use the same envelopes: a browser session is
//! one more peer of the relay.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How often a participant repeats [`Envelope::VoiceJoin`](crate::Envelope::VoiceJoin).
pub const HEARTBEAT: Duration = Duration::from_secs(5);
/// A participation ends when its device has not repeated its join for this
/// long.
pub const PARTICIPATION_TTL: Duration = Duration::from_secs(15);
/// Participants of one call at most: the voice carrier subscribes to at most
/// seven other publishers.
pub const MAX_PARTICIPANTS: usize = 8;

/// What a call's participants may do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Directions {
    /// Send audio.
    pub publish: bool,
    /// Hear the other participants.
    pub subscribe: bool,
}

/// A change to a room's calls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallOperation {
    /// Start a call on voice channel `channel_id` of the chat record
    /// `record_id` of app `app_id`.
    Start {
        /// The chat app.
        app_id: String,
        /// The chat record holding the channel.
        record_id: String,
        /// The voice channel.
        channel_id: String,
        /// What participants may do.
        allowed: Directions,
    },
    /// End an active call.
    End {
        /// The call.
        call: [u8; 32],
        /// The epoch the caller last saw.
        epoch: u64,
    },
    /// Restart an ended call on its original channel.
    Restart {
        /// The call.
        call: [u8; 32],
        /// The epoch the caller last saw.
        epoch: u64,
    },
    /// Change what participants may do.
    SetDirections {
        /// The call.
        call: [u8; 32],
        /// The epoch the caller last saw.
        epoch: u64,
        /// What participants may do from now on.
        allowed: Directions,
    },
    /// Deny one person directions in a call; denying neither lifts it.
    SetDenial {
        /// The call.
        call: [u8; 32],
        /// The epoch the caller last saw.
        epoch: u64,
        /// The person's user root.
        person: [u8; 32],
        /// What the person may no longer do.
        denied: Directions,
    },
}

/// A call as the room's governance records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallState {
    /// The call.
    pub call: [u8; 32],
    /// Advances with every change of the call.
    pub epoch: u64,
    /// Whether devices may join.
    pub active: bool,
    /// What participants may do.
    pub allowed: Directions,
}

/// Admission of one participation to a call's voice carrier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceGrant {
    /// The call.
    pub call: [u8; 32],
    /// The call's epoch this grant was issued under.
    pub epoch: u64,
    /// This device's participation: repeat it in every heartbeat.
    pub participation: [u8; 32],
    /// Opaque token for the carrier's admission preface.
    #[serde(with = "serde_bytes")]
    pub token: Vec<u8>,
    /// `https://host:port/moq/voice` of the carrier.
    pub endpoint: String,
    /// SHA-256 (lowercase hex) of the carrier's development certificate to
    /// pin; empty to verify it with the system roots.
    pub fingerprints: Vec<String>,
    /// What this participation may do now.
    pub directions: Directions,
    /// The namespace this participation publishes, when it may publish.
    pub publication: Option<String>,
    /// The namespaces of the other participants it hears, when it may
    /// subscribe.
    pub subscriptions: Vec<String>,
    /// Everyone in the call, this participation included.
    pub participants: Vec<VoiceParticipant>,
    /// The token admits no new carrier after this.
    pub expires_at_ns: u64,
}

/// A call of a room, for a device deciding whether to join it
/// ([`Envelope::VoiceCalls`](crate::Envelope::VoiceCalls)).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSummary {
    /// The call as governance records it.
    pub state: CallState,
    /// The chat app the call's voice channel belongs to.
    pub app_id: String,
    /// The chat record holding the channel.
    pub record_id: String,
    /// The voice channel.
    pub channel_id: String,
    /// Who is in the call now.
    pub participants: Vec<VoiceParticipant>,
}

/// One participant of a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceParticipant {
    /// The participating device.
    pub device_public_key: [u8; 32],
    /// Its person.
    pub user_public_key: [u8; 32],
    /// Its voice namespace.
    pub namespace: String,
    /// Whether it may publish.
    pub publishes: bool,
}

/// The voice namespace of `participation` in `call`: `<call hex>/<participation hex>`.
#[must_use]
pub fn namespace(call: &[u8; 32], participation: &[u8; 32]) -> String {
    format!("{}/{}", hex::encode(call), hex::encode(participation))
}
