//! Portable journal records shared by review and retained Home recovery.
//! Records alone grant no filesystem authority.
use crate::migration_durability as durable;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub identity: durable::Identity,
    pub digest: [u8; 32],
    pub mode: u32,
    pub directory: bool,
}
