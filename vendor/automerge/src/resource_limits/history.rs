//! Guarded history metadata and canonical change export.
//!
//! OWNER: lane L3.
mod export;

use super::custody::{NativeOwned, NativeWorkTurn};
use super::dims::{self, DocDims, IncomingDims};
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
    ///
    /// Same selection as `Automerge::get_changes(heads)`: unknown hashes in
    /// `heads` are ignored. The whole-history export peak is reserved before
    /// any selection state exists; the returned changes keep a reservation
    /// of their retained bound and the rest of the peak is released.
    pub fn changes_since_bounded(
        &self,
        heads: &[ChangeHash],
        turn: &NativeWorkTurn,
    ) -> Result<NativeChanges, NativeResourceError> {
        let allowance = self.check_turn(turn)?;
        if self.pending_ops() != 0
            || self.value.native_read_is_isolated()
            || cfg!(feature = "slow_path_assertions")
        {
            return Err(NativeResourceError::Unaccounted);
        }
        let doc = &self.value.doc;
        let dims = DocDims::of(&self.value);
        let mut peak = allowance.reserve(dims::history_peak_bound(&dims, dims.changes)?)?;
        let changes = export::changes_since(&doc.ops, &doc.change_graph, heads)?;
        let retained = dims::incoming_changes_bound(&IncomingDims::from_changes(&changes)?)?;
        if retained > peak.bytes() {
            return Err(NativeResourceError::Unaccounted);
        }
        let reservation = peak.split_off(retained)?;
        Ok(NativeChanges {
            changes,
            _reservation: reservation,
        })
    }

    /// Copies committed heads without opening or committing a transaction.
    pub fn base_heads_bounded(
        &self,
        turn: &NativeWorkTurn,
    ) -> Result<NativeHashes, NativeResourceError> {
        let allowance = self.check_turn(turn)?;
        if self.pending_ops() != 0 || self.value.native_read_is_isolated() {
            return Err(NativeResourceError::Unaccounted);
        }
        let graph = &self.value.doc.change_graph;
        let count = graph.heads().count();
        let bytes = count
            .checked_mul(std::mem::size_of::<ChangeHash>())
            .ok_or(NativeResourceError::WorkBytes)?;
        let reservation = allowance.reserve(bytes)?;
        let mut hashes = Vec::new();
        hashes
            .try_reserve_exact(count)
            .map_err(|_| NativeResourceError::WorkBytes)?;
        if hashes.capacity() > count {
            return Err(NativeResourceError::Unaccounted);
        }
        hashes.extend(graph.heads());
        Ok(NativeHashes {
            hashes,
            _reservation: reservation,
        })
    }

    /// Tests for dependency hashes absent from both integrated and queued work.
    /// This read never commits pending work or allocates a hash collection.
    pub fn has_missing_dependencies(&self) -> bool {
        let document = &self.value.doc;
        document
            .queue
            .iter()
            .flat_map(|change| change.deps())
            .any(|hash| !document.change_graph.has_change(hash) && !document.queue.has_hash(hash))
    }
}
