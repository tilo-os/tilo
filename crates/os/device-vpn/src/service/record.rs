//! What the helper has set up, kept where the next helper finds it.

use serde::{Deserialize, Serialize};

use crate::protocol::{ClientLink, ExitLink, ProfileBlock};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRecord {
    pub owner: u32,
    pub link: ClientLink,
    pub dns_set: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitRecord {
    pub owner: u32,
    pub link: ExitLink,
    pub enabled_forwarding: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileBlockRecord {
    pub owner: u32,
    pub block: ProfileBlock,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub client: Option<ClientRecord>,
    pub exit: Option<ExitRecord>,
    #[serde(default)]
    pub profile_block: Option<ProfileBlockRecord>,
}

impl Record {
    pub fn dns_set(&self) -> bool {
        self.client.as_ref().is_some_and(|client| client.dns_set)
    }

    /// The record in `bytes`; an unreadable one is empty, since the helper
    /// cannot undo what it cannot read (and DNS has its own marker).
    pub fn parse(bytes: &[u8]) -> Self {
        serde_json::from_slice(bytes).unwrap_or_default()
    }
}
