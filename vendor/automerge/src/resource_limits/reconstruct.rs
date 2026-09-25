//! Detached checked load of normalized storage.
//!
//! OWNER: lane L3.
use super::custody::{NativeAllocationSet, NativeOwned, NativeWorkTurn};
use super::{NativeResourceError, NormalizedStorage};
use crate::AutoCommit;

impl NormalizedStorage<'_> {
    /// Reconstruct a detached native document under the current owner turn.
    /// The load peak (`dims::load_peak_bound` of the inspected shape) is
    /// reserved before loading; the result is adopted into custody.
    pub fn reconstruct(
        self,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<NativeOwned<AutoCommit>, NativeResourceError> {
        let _ = (allocations, turn);
        todo!("L3")
    }
}
