//! The bots of a room, as the room's governance records them.

use serde::{Deserialize, Serialize};

/// One bot of a room: one it runs, or one a member asked the room's host to
/// run that does not run yet. Bots are participants like people and agents,
/// so every surface that lists who is in a room lists them, tagged as bots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomBot {
    /// The name it shows; its directory entry while it does not run yet.
    pub name: String,
    /// Its installation (the `Subject::Bot` id), once installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation: Option<String>,
    /// Its bot device's key, hex, once installed: what its presence and its
    /// changes are signed with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The registry directory entry it comes from, when it comes from one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// The version installed, else the version asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The person who installed it or asked for it, user root hex.
    pub added_by: String,
    /// A member asked the room's host for it: taking the request back is
    /// what removes it.
    #[serde(default)]
    pub requested: bool,
    /// It starts nothing until it is resumed.
    #[serde(default)]
    pub paused: bool,
}

impl RoomBot {
    /// Whether `participant` (a presence id: a device or person key, hex, or
    /// a name) is this bot.
    #[must_use]
    pub fn is(&self, participant: &str) -> bool {
        self.device.as_deref() == Some(participant)
            || self.installation.as_deref() == Some(participant)
    }
}
