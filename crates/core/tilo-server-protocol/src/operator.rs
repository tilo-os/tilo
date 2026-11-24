//! The wait-list mailbox and status a public workspace's entry keeps: where the
//! mail about an entry goes, and where a request left at the entry stands for
//! whoever left it.
//!
//! The JSON-lines protocol a server once spoke with each workspace's child
//! process went with the second binary: the device's daemon serves the entry
//! itself, and no process speaks it.

use serde::{Deserialize, Serialize};

/// Where a mail about a wait-list entry goes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitListMailbox {
    /// The address, as typed.
    pub contact: String,
    /// The language the mail is in (`en` or `de`).
    pub language: String,
}

impl std::fmt::Debug for WaitListMailbox {
    /// Without the address, which no log names.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitListMailbox")
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

/// Where a request left at the entry stands, for whoever left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WaitListStatus {
    /// Left, its address not confirmed yet.
    Pending,
    /// On the list.
    Listed,
    /// On the list, and invited.
    Invited,
    /// Not on the list (taken off, joined, or never confirmed).
    Gone,
}
