//! Detached checked load of normalized storage.
//!
//! OWNER: lane L3.
//!
//! The load peak is reserved from the inspected [`NativeShape`] before any
//! native parse state exists; the unmodified 0.12 load primitives then run
//! under that reservation and the result is adopted into custody (which
//! fails closed if the loaded document's retained bound exceeds the peak).
mod load;

use super::custody::{self, NativeAllocationSet, NativeOwned, NativeWorkTurn};
use super::dims::{self, IncomingDims};
use super::{NativeResourceError, NativeShape, NormalizedStorage};
use crate::AutoCommit;

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

/// Change-collector encoder slots that the 0.12 load path sizes from
/// *declared* change metadata rather than from ops actually present.
///
/// `ChangeCollector::try_from_change_meta` creates one `ChangeBuilder` per
/// declared change and gives it an `OpEncoderStrategy` sized by the declared
/// `num_ops = max_op - parent max_op` (document chunks) or
/// `max_op + 1 - start_op` (bundles), before a single op is visited:
///
/// - native targets: `vec![None; num_ops]` of `Option<OpBuilder>` while that
///   is no larger than a `ProgressiveEncoder`, otherwise one boxed
///   `ProgressiveEncoder`, so at most `size_of::<ProgressiveEncoder>()` per
///   declared change;
/// - wasm (`CAN_OOM`): always the dense `Vec<Option<OpBuilder>>`.
///
/// The inspector bounds the declared slot total as
/// `NativeShape::dense_operations <= NativeLimits::max_operations`, but that
/// dimension is not part of [`IncomingDims`], so it is reserved here, fail
/// closed, on top of `dims::load_peak_bound`. Both terms are charged on every
/// target (the larger of the two applies per change).
fn declared_collector_slots(shape: &NativeShape) -> Result<usize> {
    use crate::op_set2::change::collector::ProgressiveEncoder;
    use crate::op_set2::OpBuilder;
    use std::mem::size_of;
    let dense = shape
        .dense_operations
        .checked_mul(size_of::<Option<OpBuilder<'static>>>())
        .ok_or(NativeResourceError::WorkBytes)?;
    let progressive = shape
        .changes
        .checked_mul(size_of::<ProgressiveEncoder<'static>>() + size_of::<usize>())
        .ok_or(NativeResourceError::WorkBytes)?;
    dense
        .checked_add(progressive)
        .ok_or(NativeResourceError::WorkBytes)
}

impl NormalizedStorage<'_> {
    /// Reconstruct a detached native document under the current owner turn.
    /// The load peak (`dims::load_peak_bound` of the inspected shape) is
    /// reserved before loading; the result is adopted into custody.
    pub fn reconstruct(
        self,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<NativeOwned<AutoCommit>> {
        let allowance = allocations.check_turn(turn)?;
        // The normalized bytes' own credit must come from this exact turn:
        // otherwise their buffer would be retained under another ledger.
        if !turn.accepts_reservation(&self._reservation) {
            return Err(NativeResourceError::Unaccounted);
        }
        let incoming = IncomingDims::from_shape(&self.shape);
        let bound = dims::load_peak_bound(&incoming)?
            .checked_add(declared_collector_slots(&self.shape)?)
            .ok_or(NativeResourceError::WorkBytes)?;
        let peak = allowance.reserve(bound)?;
        let doc = load::load_strict(&self.bytes)?;
        // The complete load call has returned, so its parse, collector,
        // encoder, index and queued-apply scratch has died. `peak` stays held
        // until custody transfers the retained part of it.
        let value = AutoCommit::native_from_doc(doc);
        custody::adopt(value, allocations, turn, peak)
    }
}
