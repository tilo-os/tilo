//! What the helper has set up, kept where the next helper finds it.

use serde::{Deserialize, Serialize};

use crate::protocol::{ClientLink, ExitLink, ProfileBlock};

/// A client tunnel the helper set up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRecord {
    /// The person whose shell opened it, and whose traffic it routes.
    pub owner: u32,
    pub link: ClientLink,
    /// Whether `NetworkManager`'s global DNS points at the tunnel.
    pub dns_set: bool,
}

/// An exit the helper set up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitRecord {
    pub owner: u32,
    pub link: ExitLink,
    /// Whether forwarding was off before, so the helper turns it off again.
    pub enabled_forwarding: bool,
}

/// The block of the person's VPN profiles the helper holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileBlockRecord {
    /// The person whose shell asked for it.
    pub owner: u32,
    pub block: ProfileBlock,
}

/// Everything the helper has set up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub client: Option<ClientRecord>,
    pub exit: Option<ExitRecord>,
    #[serde(default)]
    pub profile_block: Option<ProfileBlockRecord>,
}

impl Record {
    /// Whether `NetworkManager`'s DNS needs putting back.
    pub fn dns_set(&self) -> bool {
        self.client.as_ref().is_some_and(|client| client.dns_set)
    }

    /// The record in `bytes`; an unreadable one is empty, since the helper
    /// cannot undo what it cannot read (and DNS has its own marker).
    pub fn parse(bytes: &[u8]) -> Self {
        serde_json::from_slice(bytes).unwrap_or_default()
    }
}
