//! Clipboard continuity as one of the person's devices lets another have what it copied: the
//! copies the person's other devices offer, which this device's daemon carries between them
//! ([`tilo_runtime::device::clipboard`]).
//!
//! The copies themselves are the daemon's: it takes what its session shares, keeps what another
//! of the person's devices shares with it, and publishes the reading below over the shell link.
//! This module holds the reading's shape and the resource's name, which the daemon and the
//! session both use.

use serde::{Deserialize, Serialize};

/// The resource the daemon publishes the copies the person's other devices offer as.
pub const RESOURCE: &str = "session:clipboard-continuity";

/// One copy another of the person's devices shares, as its daemon carries it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCopy {
    /// The device that copied it, by its public key as 64 lowercase hex digits.
    pub origin: String,
    /// Its format.
    pub mime: String,
    /// Its bytes, hexadecimal.
    pub bytes: String,
    /// When it was copied, in Unix milliseconds.
    #[serde(default)]
    pub created_unix_ms: u64,
    /// Whether the person keeps it when the history is cleared.
    #[serde(default)]
    pub pinned: bool,
}

/// What the resource reads: every copy the person's other devices offered, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reading {
    #[serde(default)]
    pub copies: Vec<RemoteCopy>,
}
