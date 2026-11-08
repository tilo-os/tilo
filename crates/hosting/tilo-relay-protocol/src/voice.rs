//! Voice calls in the workspaces a relay governs.
//!
//! A call belongs to a workspace's governance: the workspace's governor records each
//! call, bound to one voice channel of a chat record, with the directions its
//! participants may use and per-person denials.
//!
//! Joining a voice channel is joining its call, as in any voice chat: a
//! member opens the channel's call with [`CallOperation::Open`], which starts
//! it when the channel has none yet (or its call was ended), and then joins
//! it. Nobody has to start a call first; the call is empty once the last
//! participant leaves. A workspace's owner or an administrator ends and moderates
//! calls with the other operations of
//! [`Envelope::VoiceCall`](crate::Envelope::VoiceCall).
//!
//! A device subscribed to the workspace joins an active call with
//! [`Envelope::VoiceJoin`](crate::Envelope::VoiceJoin) and gets
//! a [`VoiceGrant`]: its participation and the roster of everyone in the call.
//! It repeats the join with its participation every [`HEARTBEAT`]; a
//! participation not heard from for [`PARTICIPATION_TTL`] ends. When the
//! roster changes, the relay sends each remaining participant a fresh
//! grant. The call's media does not travel through the relay: the members'
//! devices publish it on the workspace's live plane (`tilo-live`), which the
//! workspace's keys seal.
//!
//! A device that does not read the workspace's governance itself (a browser
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

/// A change to a workspace's calls.
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

/// A call as the workspace's governance records it.
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

/// Admission of one participation to a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceGrant {
    pub call: [u8; 32],
    /// The call's epoch this grant was issued under.
    pub epoch: u64,
    /// This device's participation: repeat it in every heartbeat.
    pub participation: [u8; 32],
    /// What this participation may do now.
    pub directions: Directions,
    /// Everyone in the call, this participation included.
    pub participants: Vec<VoiceParticipant>,
}

/// A call of a workspace, for a device deciding whether to join it
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
    pub publishes: bool,
    /// What it shows besides its voice.
    pub showing: Showing,
}

/// What a participant shows besides its voice: its pictures travel the
/// workspace's live plane while it shows them.
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
