//! Voice calls in the rooms a relay governs.
//!
//! A call belongs to a room's governance: the room's governor records each
//! call, bound to one voice channel of a chat record, with the directions its
//! participants may use and per-person denials.
//!
//! Joining a voice channel is joining its call, as in any voice chat: a
//! member opens the channel's call with [`CallOperation::Open`], which starts
//! it when the channel has none yet (or its call was ended), and then joins
//! it. Nobody has to start a call first; the call is empty once the last
//! participant leaves. A room's owner or an administrator ends and moderates
//! calls with the other operations of
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
//! What a participant shows besides its voice (its camera, a shared screen,
//! whether its microphone is off or it is deafened) is [`Showing`], set with
//! [`Envelope::VoiceShow`](crate::Envelope::VoiceShow) and carried on its
//! [`VoiceParticipant`] in every grant and call list, so everyone sees who
//! has a camera on, shares a screen or is muted before any picture arrives.
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
    /// Open the call of voice channel `channel_id` of the chat record
    /// `record_id` of app `app_id`, to join it: its active call, the call it
    /// had restarted, or a new one where participants publish and subscribe.
    /// Any member may open a channel's call.
    Open {
        app_id: String,
        record_id: String,
        channel_id: String,
    },
    /// Start a call on voice channel `channel_id` of the chat record
    /// `record_id` of app `app_id`.
    Start {
        app_id: String,
        record_id: String,
        channel_id: String,
        /// What participants may do.
        allowed: Directions,
    },
    /// End an active call.
    End {
        call: [u8; 32],
        /// The epoch the caller last saw.
        epoch: u64,
    },
    /// Restart an ended call on its original channel.
    Restart {
        call: [u8; 32],
        /// The epoch the caller last saw.
        epoch: u64,
    },
    /// Change what participants may do.
    SetDirections {
        call: [u8; 32],
        /// The epoch the caller last saw.
        epoch: u64,
        /// What participants may do from now on.
        allowed: Directions,
    },
    /// Deny one person directions in a call; denying neither lifts it.
    SetDenial {
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
    pub app_id: String,
    pub record_id: String,
    pub channel_id: String,
    pub participants: Vec<VoiceParticipant>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceParticipant {
    /// The participating device.
    pub device_public_key: [u8; 32],
    /// Its person.
    pub user_public_key: [u8; 32],
    pub namespace: String,
    pub publishes: bool,
    /// What it shows besides its voice.
    pub showing: Showing,
    /// Set when a bridge holds this participation: for a person on another
    /// service, whose voice it carries into the call, or for the bridge
    /// itself, listening to carry the call out. Its person is then the
    /// bridge's, never the one it shows.
    pub bridged: Option<Bridged>,
}

/// Who a bridge's participation stands for. A bridge (a room's Discord bot)
/// holds one participation per person on the other side, so each is heard,
/// shown and turned down on their own, and one of its own that hears the
/// call and carries it out. A participant reading one shows it by `name`,
/// marked as reached through `service`, and never as the bridge's person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bridged {
    /// The service, lowercase: `discord`.
    pub service: String,
    /// The account on that service whose voice this carries; `None` for
    /// the bridge's own participation, which carries the call out.
    pub account: Option<String>,
    /// The shown name: trimmed, NFC-normalized, 1..=80 characters, as an
    /// appearance's (`tilo_actions::appearance`).
    pub name: String,
    /// BLAKE3 hex of the avatar in the room's content store.
    pub avatar: Option<String>,
    /// The room member the account is linked to: the participant is shown
    /// as that member, reached through `service`.
    pub linked: Option<[u8; 32]>,
}

/// What a participant shows besides its voice: its pictures are published
/// on the carrier while it shows them (tilo-video/1, `tilo_moq::video`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is its own fact the others see: camera, screen, muted, deafened"
)]
pub struct Showing {
    pub camera: bool,
    /// It shares a screen.
    pub screen: bool,
    /// Its microphone is off: everyone sees it muted. A participant that may
    /// not publish counts as muted.
    pub muted: bool,
    /// It hears nothing of the call (and is muted): everyone sees it
    /// deafened.
    pub deafened: bool,
}

/// The call that is room `room`'s window carrier: every member device of a
/// room that shares its layout joins it ([`Envelope::VoiceJoin`](crate::Envelope::VoiceJoin))
/// to publish the windows of host programs it runs (a terminal, any Wayland
/// or X client) and to view the other members' as live video, one
/// `tilo-video/1` window track each. It is in no room's governance: it
/// stands while the room shares its layout, and a device holds it beside its
/// voice call, never in place of one. Its pictures are sealed for the
/// room's members, so the relay forwards what it cannot read.
#[must_use]
pub fn windows_call(room: &str) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-room-windows-v1\0");
    hash.update(room.as_bytes());
    *hash.finalize().as_bytes()
}

/// The voice namespace of `participation` in `call`: `<call hex>/<participation hex>`.
#[must_use]
pub fn namespace(call: &[u8; 32], participation: &[u8; 32]) -> String {
    format!("{}/{}", hex::encode(call), hex::encode(participation))
}
