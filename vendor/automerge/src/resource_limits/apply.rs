//! Checked, atomic admission of incoming changes.
//!
//! OWNER: lane L2 (together with
//! `op_set2/change/batch/native_validation.rs`). Types and signatures are
//! the 0.8 patch's public contract; keep them.
pub(crate) mod budget;
mod parse;
pub(crate) mod preflight;

use super::custody::{self, NativeAllocationSet, NativeOwned, NativeWorkTurn};
use super::dims::{self, DocDims, IncomingDims};
use super::{NativeAllowance, NativeLimits, NativeResourceError, NormalizedStorage};
use crate::transaction::Transactable;
use crate::{AutoCommit, Change};
use budget::Charge;

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
        self.check_turn(turn)?;
        let allowance = allocations.check_turn(turn)?;
        if !self.value.native_mutation_is_closed() {
            return Err(NativeMutationError::Busy);
        }
        if !turn.accepts_reservation(&input._reservation) {
            return Err(NativeResourceError::Unaccounted.into());
        }
        let mut charge = Charge::new(allowance);
        preflight::validate(input.bytes(), &mut charge)?;
        let shape = IncomingDims::from_shape(input.shape());
        // The retained parsed changes, plus the load-shaped expansion of any
        // document or bundle chunk, are reserved before parsing.
        let mut parse_bytes = dims::incoming_changes_bound(&shape)?;
        if preflight::has_expanding_chunks(input.bytes())? {
            parse_bytes = budget::sum(parse_bytes, dims::load_peak_bound(&shape)?)?;
        }
        charge.grow(parse_bytes)?;
        let changes = parse::changes(input.bytes(), input.shape().changes)?;
        drop(input);

        let doc = &self.value.doc;
        let validated = crate::op_set2::change::batch::native_validation::validate_incoming(
            doc,
            &changes,
            &mut charge,
        )?;
        if validated.orphaned && policy == NativeDependencyPolicy::RequireComplete {
            return Ok(NativeApplyResult::MissingDependencies);
        }
        if validated.integrated == 0 && !validated.orphaned {
            // Every supplied change is already integrated: nothing to mutate.
            return Ok(NativeApplyResult::Applied {
                document: self,
                changed: false,
                missing_dependencies: false,
            });
        }
        let incoming = IncomingDims::from_changes(&changes)?;
        let peak = dims::apply_peak_bound(&DocDims::of(&self.value), &incoming)?;
        let peak = turn.reserve(peak)?;
        let mut owned = self;
        // Validation covered the exact queue and input, so batch apply cannot
        // take its duplicate-sequence refusal after partially mutating.
        owned
            .value
            .doc
            .apply_changes_batch(changes)
            .map_err(|_| NativeMutationError::InvalidOperation)?;
        drop(charge);
        let missing_dependencies = !owned.value.doc.queue.is_empty();
        let document = custody::readopt(owned, turn, peak)?;
        Ok(NativeApplyResult::Applied {
            document,
            changed: validated.integrated != 0,
            missing_dependencies,
        })
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
    ///
    /// Unlike [`NativeOwned::apply_checked_bounded`], parsing and the linear
    /// part of batch work are bounded by the inspected `limits` rather than
    /// reserved worst-case layouts. Normalization, validation scratch and the
    /// super-linear change-graph growth of the batch are charged to
    /// `allowance` and released before returning.
    pub fn apply_incoming_checked(
        &mut self,
        input: &[u8],
        limits: &NativeLimits,
        allowance: &NativeAllowance,
        policy: NativeDependencyPolicy,
    ) -> Result<NativeIncomingOutcome, NativeMutationError> {
        if self.pending_ops() != 0 || self.native_read_is_isolated() {
            return Err(NativeMutationError::Busy);
        }
        let registry = NativeAllocationSet::new(allowance.clone())?;
        let turn = registry.work_turn(allowance.clone())?;
        let input = turn.normalize_input(input, limits)?;
        let mut charge = Charge::new(allowance);
        preflight::validate(input.bytes(), &mut charge)?;
        let changes = parse::changes(input.bytes(), input.shape().changes)?;
        drop(input);
        let validated = crate::op_set2::change::batch::native_validation::validate_incoming(
            &self.doc,
            &changes,
            &mut charge,
        )?;
        drop(charge);
        if validated.orphaned && policy == NativeDependencyPolicy::RequireComplete {
            return Ok(NativeIncomingOutcome::MissingDependencies);
        }
        if validated.integrated == 0 && !validated.orphaned {
            return Ok(NativeIncomingOutcome::Applied { changed: false });
        }
        let _growth = allowance.reserve(superlinear_growth(self, &changes)?)?;
        // Validation covered the exact queue and input, so batch apply cannot
        // take its duplicate-sequence refusal after partially mutating.
        self.apply_changes_batch(changes)
            .map_err(|_| NativeMutationError::InvalidOperation)?;
        Ok(NativeIncomingOutcome::Applied {
            changed: validated.integrated != 0,
        })
    }
}

/// Super-linear change-graph growth of applying `changes` (fragment nodes
/// whose dependency lists grow with every earlier fragment, and clock-cache
/// entries of actor width), computed exactly from the parsed changes.
fn superlinear_growth(doc: &AutoCommit, changes: &[Change]) -> Result<usize, NativeResourceError> {
    dims::superlinear_apply_bound(&DocDims::of(doc), &IncomingDims::from_changes(changes)?)
}
