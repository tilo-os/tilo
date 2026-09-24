//! Guarded history metadata and canonical change export.
use super::{NativeOwned, NativeVisit, NativeWorkTurn, Result, Visitor, mul, split};
use crate::resource_limits::{NativeReservation, NativeResourceError};
use crate::{AutoCommit, Change, ChangeHash};

/// Fixed-size hashes whose owned output remains covered until it is dropped.
#[derive(Debug)]
pub struct NativeHashes {
    hashes: Vec<ChangeHash>,
    _reservation: NativeReservation,
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
    changes: Vec<Change>,
    _reservation: NativeReservation,
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

// The private collector returns its full precharged peak. Keep the generated
// owners before that peak in drop order, including measurement/error paths.
fn retain_changes(changes: Vec<Change>, peak: NativeReservation) -> Result<NativeChanges> {
    struct Produced {
        changes: Vec<Change>,
        peak: NativeReservation,
    }
    let mut produced = Produced { changes, peak };
    let mut visitor = Visitor {
        owned: 0,
        references: 0,
        backing: None,
    };
    produced.changes.native_visit(&mut visitor)?;
    // Canonically generated Changes own their buffers/actors outright. A new
    // shared-backed representation requires actual lease custody, not a count.
    if visitor.references != 0 || visitor.owned > produced.peak.bytes() {
        return Err(NativeResourceError::Unaccounted);
    }
    let reservation = split(&mut produced.peak, visitor.owned);
    Ok(NativeChanges {
        changes: produced.changes,
        _reservation: reservation,
    })
}

impl NativeOwned<AutoCommit> {
    /// Reconstruct canonical changes absent from the supplied dependency heads.
    pub fn changes_since_bounded(
        &self,
        heads: &[ChangeHash],
        turn: &NativeWorkTurn,
    ) -> Result<NativeChanges> {
        let allowance = self.check_turn(turn)?;
        if self.pending_ops() != 0
            || self.value.native_read_is_isolated()
            || cfg!(feature = "slow_path_assertions")
            || hexane::diagnostic_exports_enabled()
        {
            return Err(NativeResourceError::Unaccounted);
        }
        let (changes, peak) = crate::op_set2::change::ChangeCollector::native_export_bounded(
            self.value.doc.ops(),
            self.value.doc.changes(),
            heads,
            allowance,
        )?;
        retain_changes(changes, peak)
    }

    /// Copies committed heads without opening or committing a transaction.
    pub fn base_heads_bounded(&self, turn: &NativeWorkTurn) -> Result<NativeHashes> {
        let allowance = self.check_turn(turn)?;
        if self.pending_ops() != 0 || self.value.native_read_is_isolated() {
            return Err(NativeResourceError::Unaccounted);
        }
        let graph = self.value.doc.changes();
        let count = graph.heads().count();
        let bytes = mul(count, std::mem::size_of::<ChangeHash>())?;
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
            .any(|hash| !document.changes().has_change(hash) && !document.queue.has_hash(hash))
    }
}
