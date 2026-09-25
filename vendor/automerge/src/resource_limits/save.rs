//! Bounded export.
//!
//! OWNER: lane L3.
use super::custody::{NativeCloneCandidate, NativeOwned, NativeWorkTurn};
use super::dims::{self, DocDims};
use super::{NativeReservation, NativeResourceError};
use crate::storage::{CompressConfig, Document};
use crate::{AutoCommit, SaveOptions};

/// Default complete-output ceiling for `save_bounded` (0.8 value).
pub(crate) const MAX_EXPORT: usize = 64 * 1024 * 1024;

/// Saved bytes with inseparable output allocation custody.
pub struct NativeSaved {
    pub(crate) bytes: Vec<u8>,
    pub(crate) _reservation: NativeReservation,
}
impl NativeSaved {
    /// Borrows the export while its allocation custody remains alive.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl std::fmt::Debug for NativeSaved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSaved")
            .field("len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

/// Bounded document export.
///
/// Semantics carried from the 0.8 patch's `native_save`: an open transaction
/// or an isolated view is refused as `Unaccounted` (saving would have to
/// commit or change the view), as is a document with actors that no change
/// uses (0.12's `save_with_options` would panic or mutate to drop them). With
/// `retain_orphans` the raw bytes of every queued change follow the document
/// chunk verbatim, exactly as `Automerge::save_with_options` appends them.
///
/// Allocation order: the save workspace (`dims::save_peak_bound`) is reserved
/// before the 0.12 encoder runs. The complete chosen representation's length
/// is then known without a destination; one short of it is refused with
/// `WorkBytes` before the destination is reserved or allocated. The
/// destination is reserved and allocated at exactly that length, and the
/// returned custody retains exactly its capacity.
pub(crate) fn save(
    doc: &AutoCommit,
    turn_allowance: &super::NativeAllowance,
    options: SaveOptions,
    maximum_output_bytes: usize,
) -> Result<NativeSaved, NativeResourceError> {
    if doc.native_has_transaction() || doc.native_read_is_isolated() {
        return Err(NativeResourceError::Unaccounted);
    }
    let am = &doc.doc;
    if am.ops.actors.len() != am.change_graph.actor_ids().count() {
        return Err(NativeResourceError::Unaccounted);
    }
    let dims = DocDims::of(doc);
    let output_bound = dims::save_output_bound(&dims)?;
    let workspace = turn_allowance.reserve(dims::save_peak_bound(&dims)?)?;
    let compress = if options.deflate {
        CompressConfig::Threshold(crate::storage::change::DEFLATE_MIN_SIZE)
    } else {
        CompressConfig::None
    };
    let body = Document::new(&am.ops, &am.change_graph, compress).into_bytes();
    let mut length = body.len();
    if options.retain_orphans {
        for change in am.queue.iter() {
            length = length
                .checked_add(change.raw_bytes().len())
                .ok_or(NativeResourceError::WorkBytes)?;
        }
    }
    if length > output_bound {
        // The dimension bound was wrong: fail closed rather than hand out
        // output that the reserved workspace may not have covered.
        return Err(NativeResourceError::Unaccounted);
    }
    if length > maximum_output_bytes {
        return Err(NativeResourceError::WorkBytes);
    }
    let reservation = turn_allowance.reserve(length)?;
    let orphans = options.retain_orphans && !am.queue.is_empty();
    let bytes = if !orphans && body.capacity() == length {
        body
    } else {
        let mut output = Vec::new();
        output
            .try_reserve_exact(length)
            .map_err(|_| NativeResourceError::WorkBytes)?;
        if output.capacity() != length {
            return Err(NativeResourceError::Unaccounted);
        }
        output.extend_from_slice(&body);
        drop(body);
        if orphans {
            for change in am.queue.iter() {
                output.extend_from_slice(change.raw_bytes());
            }
        }
        output
    };
    if bytes.len() != length || bytes.capacity() != length {
        return Err(NativeResourceError::Unaccounted);
    }
    drop(workspace);
    Ok(NativeSaved {
        bytes,
        _reservation: reservation,
    })
}

impl NativeOwned<AutoCommit> {
    /// Exports under an additional complete-output ceiling, checked before
    /// allocating the destination. Workspace uses the current turn's credit.
    pub fn save_bounded_with_limit(
        &self,
        turn: &NativeWorkTurn,
        options: SaveOptions,
        maximum_output_bytes: usize,
    ) -> Result<NativeSaved, NativeResourceError> {
        save(
            &self.value,
            self.check_turn(turn)?,
            options,
            maximum_output_bytes,
        )
    }
    /// Exports a closed document using the owner's current work turn.
    pub fn save_bounded(
        &self,
        turn: &NativeWorkTurn,
        options: SaveOptions,
    ) -> Result<NativeSaved, NativeResourceError> {
        self.save_bounded_with_limit(turn, options, MAX_EXPORT)
    }
}
impl NativeCloneCandidate<AutoCommit> {
    /// Exports an unqualified candidate under a complete-output ceiling.
    pub fn save_bounded_with_limit(
        &self,
        turn: &NativeWorkTurn,
        options: SaveOptions,
        maximum_output_bytes: usize,
    ) -> Result<NativeSaved, NativeResourceError> {
        save(
            &self.value,
            self.check_turn(turn)?,
            options,
            maximum_output_bytes,
        )
    }
    /// Exports an unqualified candidate; this does not qualify its source.
    pub fn save_bounded(
        &self,
        turn: &NativeWorkTurn,
        options: SaveOptions,
    ) -> Result<NativeSaved, NativeResourceError> {
        self.save_bounded_with_limit(turn, options, MAX_EXPORT)
    }
}
