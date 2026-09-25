//! Checked, atomic admission of incoming changes.
//!
//! OWNER: lane L2 (together with
//! `op_set2/change/batch/native_validation.rs`). Types and signatures are
//! the 0.8 patch's public contract; keep them.
use super::custody::{NativeAllocationSet, NativeOwned, NativeWorkTurn};
use super::{NativeAllowance, NativeLimits, NativeResourceError, NormalizedStorage};
use crate::AutoCommit;

/// Whether valid changes with absent dependencies may remain queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeDependencyPolicy {
    RequireComplete,
    RetainOrphans,
}

/// Fixed mutation failures without native values or allocating error details.
#[derive(Debug, thiserror::Error)]
pub enum NativeMutationError {
    #[error(transparent)]
    Resource(#[from] NativeResourceError),
    #[error("native candidate has pending or isolated work")]
    Busy,
    #[error("native mutation target is invalid")]
    InvalidTarget,
    #[error("native mutation operation is invalid")]
    InvalidOperation,
    /// An actor sequence number is reused by a different change.
    #[error("duplicate seq number for an existing actor sequence")]
    DuplicateSequence,
}

/// The result of consuming a disposable native candidate.
pub enum NativeApplyResult {
    Applied {
        document: NativeOwned<AutoCommit>,
        changed: bool,
        missing_dependencies: bool,
    },
    MissingDependencies,
}
impl std::fmt::Debug for NativeApplyResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Applied {
                changed,
                missing_dependencies,
                ..
            } => f
                .debug_struct("Applied")
                .field("changed", changed)
                .field("missing_dependencies", missing_dependencies)
                .finish_non_exhaustive(),
            Self::MissingDependencies => f.write_str("MissingDependencies"),
        }
    }
}

/// Outcome of checked incoming application to an ordinary document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeIncomingOutcome {
    /// Admitted. `changed` is whether the integrated heads moved; under
    /// `RetainOrphans` some changes may instead remain queued.
    Applied { changed: bool },
    /// Some change lacks dependencies (`RequireComplete` only); the document
    /// was not modified.
    MissingDependencies,
}

impl NativeOwned<AutoCommit> {
    /// Apply complete checked storage to this disposable candidate.
    ///
    /// The candidate is consumed. On any refusal it is dropped with every
    /// charged temporary, so callers keep their separately owned original.
    /// Parse, semantic validation and the whole batch peak are each reserved
    /// in this turn before the allocations they cover.
    pub fn apply_checked_bounded(
        self,
        input: NormalizedStorage<'_>,
        policy: NativeDependencyPolicy,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> Result<NativeApplyResult, NativeMutationError> {
        let _ = (input, policy, allocations, turn);
        todo!("L2")
    }
}

impl AutoCommit {
    /// Checked admission of remote storage into an ordinary document.
    ///
    /// The input is inspected against `limits`, parsed and semantically
    /// validated against this document before any mutation. Work must fit
    /// `allowance`; otherwise nothing is applied. With `RequireComplete`,
    /// incomplete input is refused without mutation; `RetainOrphans` applies
    /// the complete part and queues the rest, like ordinary `apply_changes`.
    /// The grown document is not adopted into allocation custody.
    pub fn apply_incoming_checked(
        &mut self,
        input: &[u8],
        limits: &NativeLimits,
        allowance: &NativeAllowance,
        policy: NativeDependencyPolicy,
    ) -> Result<NativeIncomingOutcome, NativeMutationError> {
        let _ = (input, limits, allowance, policy);
        todo!("L2")
    }
}
