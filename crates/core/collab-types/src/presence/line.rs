//! What an app says of its person on the connection its launcher handed it, and what the device
//! hands it of others: the two shapes the line wire carries as JSON ([`Said`], [`Heard`]).
//!
//! An app holds no live session: its words leave as one frame under the key they are said under,
//! and the device's one live presence says them for the app's member in the app's workspace, on
//! to the other devices exactly as the shell's words go. What those devices say comes back the
//! same way, one frame each, as the workspace's live session hears it.

use serde::{Deserialize, Serialize};

use crate::MemberId;
use crate::presence::{Payload, PresenceKey};

/// One word of an app's person: what they say under `key`, as the workspace's live session
/// carries it. The app, the workspace and the member are the connection's, never the frame's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Said {
    pub key: PresenceKey,
    pub payload: Payload,
}

/// One word an app's views are shown: `member` said `payload` under `key` in the app's
/// workspace, and this device's live session heard it from another device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heard {
    pub member: MemberId,
    pub key: PresenceKey,
    pub payload: Payload,
}
