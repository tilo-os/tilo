//! Which records' writes must be sealed, as the store's order says.
//!
//! A record's writes in the clear before its first sealed write are the base
//! its sealed writes are changes over. Once a sealed write of the record is
//! in the store's order, a write to it in the clear is a downgrade: the store
//! refuses it before keeping it, so no reader is ever sent it, however it
//! later asks (live, replayed or opened).
//!
//! A channel's verdict is read once from what the store retains of it, and
//! followed from then on as writes are admitted in order; a channel whose
//! retained history is rewritten (compacted, collected, archived) is read
//! again.

use std::collections::HashMap;
use std::sync::Mutex;

use tilo_lock::MutexRecover as _;

use super::{StoreError, WorkspaceStore};

/// Whether each channel read so far holds a sealed write.
#[derive(Default)]
pub(crate) struct SealedChannels(Mutex<HashMap<[u8; 32], bool>>);

impl SealedChannels {
    fn get(&self, channel: &[u8; 32]) -> Option<bool> {
        self.0.lock_recover().get(channel).copied()
    }

    fn set(&self, channel: [u8; 32], sealed: bool) {
        self.0.lock_recover().insert(channel, sealed);
    }

    pub(super) fn forget(&self, channel: &[u8; 32]) {
        self.0.lock_recover().remove(channel);
    }
}

impl WorkspaceStore {
    /// Whether the store keeps `delta`, the next write in its order: `false`
    /// for a record write in the clear to a record whose history already holds
    /// a sealed write. A sealed write marks its record sealed from here on.
    /// Must run on the persistence owner, in the write's turn.
    ///
    /// # Errors
    ///
    /// The record's retained history cannot be read.
    pub(crate) fn keeps_in_order(&self, delta: &[u8]) -> Result<bool, StoreError> {
        let Some((channel, _, body)) = crate::coedit::retention_view(delta) else {
            return Ok(true);
        };
        let Some(sealed) = self.sealed_write(body) else {
            return Ok(true);
        };
        let sealed_channels = &self.capture_leases.sealed;
        if sealed {
            sealed_channels.set(channel, true);
            return Ok(true);
        }
        if let Some(sealed) = sealed_channels.get(&channel) {
            return Ok(!sealed);
        }
        let mut sealed = false;
        for entry in self.channel_entries(&channel)? {
            if self.sealed_write(&entry.body) == Some(true) {
                sealed = true;
                break;
            }
        }
        sealed_channels.set(channel, sealed);
        Ok(!sealed)
    }

    /// Whether `body` is a sealed record write; `None` when it is no record
    /// write at all.
    fn sealed_write(&self, body: &[u8]) -> Option<bool> {
        if !self.payloads.is_document(body) {
            return None;
        }
        let (_, op) = self.payloads.open_document(body)?;
        Some(op.encryption.is_some())
    }
}
