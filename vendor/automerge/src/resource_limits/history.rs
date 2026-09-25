//! Guarded history metadata and canonical change export.
//!
//! OWNER: lane L3.
use super::custody::{NativeOwned, NativeWorkTurn};
use super::{NativeReservation, NativeResourceError};
use crate::{AutoCommit, Change, ChangeHash};

/// Fixed-size hashes whose owned output remains covered until it is dropped.
#[derive(Debug)]
pub struct NativeHashes {
    pub(crate) hashes: Vec<ChangeHash>,
    pub(crate) _reservation: NativeReservation,
}
impl NativeHashes {
    /// Borrows the ordered hashes without releasing their output custody.
    pub fn as_slice(&self) -> &[ChangeHash] {
        &self.hashes
    }
}

/// Canonical native changes with inseparable output allocation custody.
#[derive(Debug)]
pub struct NativeChanges {
    pub(crate) changes: Vec<Change>,
    pub(crate) _reservation: NativeReservation,
}
impl NativeChanges {
    pub fn len(&self) -> usize {
        self.changes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
    pub fn raw_bytes(&self, index: usize) -> Option<&[u8]> {
        self.changes.get(index).map(Change::raw_bytes)
    }
    pub fn hash(&self, index: usize) -> Option<ChangeHash> {
        self.changes.get(index).map(Change::hash)
    }
}

impl NativeOwned<AutoCommit> {
    /// Reconstruct canonical changes absent from the supplied dependency heads.
    pub fn changes_since_bounded(
        &self,
        heads: &[ChangeHash],
        turn: &NativeWorkTurn,
    ) -> Result<NativeChanges, NativeResourceError> {
        let _ = (heads, turn);
        todo!("L3")
    }
    /// Copies committed heads without opening or committing a transaction.
    pub fn base_heads_bounded(&self, turn: &NativeWorkTurn) -> Result<NativeHashes, NativeResourceError> {
        let _ = turn;
        todo!("L3")
    }
    /// Tests for dependency hashes absent from both integrated and queued work.
    /// This read never commits pending work or allocates a hash collection.
    pub fn has_missing_dependencies(&self) -> bool {
        todo!("L3")
    }
}
