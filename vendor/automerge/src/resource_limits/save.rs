//! Bounded export.
//!
//! OWNER: lane L3.
use super::custody::{NativeCloneCandidate, NativeOwned, NativeWorkTurn};
use super::{NativeReservation, NativeResourceError};
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
        f.debug_struct("NativeSaved").field("len", &self.bytes.len()).finish_non_exhaustive()
    }
}

pub(crate) fn save(
    doc: &AutoCommit,
    turn_allowance: &super::NativeAllowance,
    options: SaveOptions,
    maximum_output_bytes: usize,
) -> Result<NativeSaved, NativeResourceError> {
    let _ = (doc, turn_allowance, options, maximum_output_bytes);
    todo!("L3")
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
        save(&self.value, self.check_turn(turn)?, options, maximum_output_bytes)
    }
    /// Exports a closed document using the owner's current work turn.
    pub fn save_bounded(&self, turn: &NativeWorkTurn, options: SaveOptions) -> Result<NativeSaved, NativeResourceError> {
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
        save(&self.value, self.check_turn(turn)?, options, maximum_output_bytes)
    }
    /// Exports an unqualified candidate; this does not qualify its source.
    pub fn save_bounded(&self, turn: &NativeWorkTurn, options: SaveOptions) -> Result<NativeSaved, NativeResourceError> {
        self.save_bounded_with_limit(turn, options, MAX_EXPORT)
    }
}
