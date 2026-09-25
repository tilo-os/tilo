//! Detached checked load of normalized storage.
//!
//! OWNER: lane L3.
//!
//! The load peak is reserved from the inspected `NativeShape` before any
//! native parse state exists; the unmodified 0.12 load primitives then run
//! under that reservation and the result is adopted into custody (which
//! fails closed if the loaded document's retained bound exceeds the peak).
mod load;

use super::apply::budget::Charge;
use super::apply::preflight;
use super::custody::{self, NativeAllocationSet, NativeOwned, NativeWorkTurn};
use super::dims::{self, IncomingDims};
use super::{NativeResourceError, NormalizedStorage};
use crate::AutoCommit;

type Result<T = ()> = std::result::Result<T, NativeResourceError>;

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
        // Borrowed column preflight (L2) over every chunk before any native
        // decoder runs: 0.12 change parsing panics in `OpId::new` on
        // counters or actor indexes above u32::MAX. Its per-chunk scratch is
        // charged to this turn and released here.
        {
            let mut charge = Charge::new(allowance);
            preflight::validate(&self.bytes, &mut charge)?;
        }
        let incoming = IncomingDims::from_shape(&self.shape);
        let bound = dims::load_peak_bound(&incoming)?;
        let mut peak = allowance.reserve(bound)?;
        let doc = load::load_strict(&self.bytes, allowance, &mut peak)?;
        // The complete load call has returned, so its parse, collector,
        // encoder, index and queued-apply scratch has died. `peak` stays held
        // until custody transfers the retained part of it.
        let value = AutoCommit::native_from_doc(doc);
        custody::adopt(value, allocations, turn, peak)
    }
}
