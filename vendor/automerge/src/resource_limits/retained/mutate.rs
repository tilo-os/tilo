//! Consuming native changes, with distinct parse and mutation custody.
pub(crate) mod columns;
mod custody;
pub(crate) mod plan;

use super::{
    NativeAllocationSet, NativeOwned, NativeVisit, NativeWorkTurn, Result, Visitor, add,
    adoption_extra_bytes, split,
};
use crate::resource_limits::reconstruct::Budget;
use crate::resource_limits::reconstruct::incoming::ParsedIncoming;
use crate::resource_limits::{NativeReservation, NativeResourceError, NormalizedStorage};
use crate::{AutoCommit, Change};
use custody::PendingMutation;
use plan::IncomingDimensions;

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

// Private incoming parser handoff. No caller obtains native changes without
// the inseparable reservation in its own drop-ordered internal holder.
pub(crate) fn retain_incoming(
    changes: Vec<Change>,
    peak: NativeReservation,
) -> Result<(Vec<Change>, NativeReservation)> {
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
    if visitor.references != 0 || visitor.owned > produced.peak.bytes() {
        return Err(NativeResourceError::Unaccounted);
    }
    let retained = split(&mut produced.peak, visitor.owned);
    Ok((produced.changes, retained))
}

impl NativeOwned<AutoCommit> {
    /// Apply complete checked storage to this disposable candidate.
    ///
    /// The candidate is consumed. On any refusal it is dropped with every
    /// charged temporary, so callers keep their separately owned original.
    /// Parse, semantic validation and the whole batch/adoption peak are each
    /// reserved in this turn before the allocations they cover.
    pub fn apply_checked_bounded(
        self,
        input: NormalizedStorage<'_>,
        policy: NativeDependencyPolicy,
        allocations: &mut NativeAllocationSet,
        turn: &NativeWorkTurn,
    ) -> std::result::Result<NativeApplyResult, NativeMutationError> {
        self.check_turn(turn)?;
        let allowance = allocations.check_turn(turn)?;
        if !self.value.native_mutation_is_closed() {
            return Err(NativeMutationError::Busy);
        }
        let parsed = ParsedIncoming::parse(input, turn)?;
        let doc = &self.value.doc;
        let validated = doc.native_validate_incoming(parsed.changes(), Some(allowance))?;
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

        let mut budget = incoming_batch_plan(doc, parsed.changes())?;
        // Final adoption revisits every shared backing of the grown value.
        let mut count = Visitor {
            owned: 0,
            references: 0,
            backing: None,
        };
        self.value.native_visit(&mut count)?;
        let references = add(count.references, budget.backings)?;
        budget.add(adoption_extra_bytes(references, allocations)?)?;
        let peak = turn.reserve(budget.bytes())?;

        let mut pending = PendingMutation::new(self, parsed, peak, turn)?;
        pending.apply()?;
        let document = pending.finish(allocations, turn)?;
        let missing_dependencies = !document.value.doc.queue.is_empty();
        Ok(NativeApplyResult::Applied {
            document,
            changed: validated.integrated != 0,
            missing_dependencies,
        })
    }
}

/// Batch workspace, graph, column and heads growth for applying `incoming`
/// together with every queued change. Final native adoption is separate.
fn incoming_batch_plan(doc: &crate::Automerge, incoming: &[Change]) -> Result<Budget> {
    let mut budget = Budget::default();
    let queued = || doc.queue.iter().chain(incoming.iter());
    let dimensions = IncomingDimensions::read(queued())?;
    doc.native_incoming_workspace_plan(&dimensions, &mut budget)?;
    doc.native_incoming_dependencies_plan(dimensions.changes, &mut budget)?;
    doc.ops.native_incoming_storage_plan(&dimensions, &mut budget)?;
    doc.change_graph
        .native_incoming_noncolumn_plan(&doc.ops.actors, queued(), &mut budget)?;
    doc.change_graph
        .native_incoming_column_plan(queued(), &mut budget)?;
    Ok(budget)
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

impl AutoCommit {
    /// Checked admission of remote storage into an ordinary document.
    ///
    /// The input is inspected against `limits`, parsed and semantically
    /// validated against this document before any mutation. Normalization must
    /// fit `allowance`; otherwise nothing is applied. With `RequireComplete`,
    /// incomplete input is refused without mutation; `RetainOrphans` applies
    /// the complete part and queues the rest, like ordinary `apply_changes`.
    ///
    /// Unlike [`NativeOwned::apply_checked_bounded`], parse and batch work are
    /// bounded by the inspected input `limits` rather than reserved worst-case
    /// layouts (which exceed realistic documents by orders of magnitude), and
    /// the grown document is not adopted into allocation custody.
    pub fn apply_incoming_checked(
        &mut self,
        input: &[u8],
        limits: &crate::resource_limits::NativeLimits,
        allowance: &crate::resource_limits::NativeAllowance,
        policy: NativeDependencyPolicy,
    ) -> std::result::Result<NativeIncomingOutcome, NativeMutationError> {
        if crate::transaction::Transactable::pending_ops(self) != 0 || self.native_read_is_isolated() {
            return Err(NativeMutationError::Busy);
        }
        let registry = NativeAllocationSet::new(allowance.clone())?;
        let turn = registry.work_turn(allowance.clone())?;
        let changes = ParsedIncoming::parse_limited(turn.normalize_input(input, limits)?, &turn)?;
        let validated = self.doc.native_validate_incoming(&changes, None)?;
        if validated.orphaned && policy == NativeDependencyPolicy::RequireComplete {
            return Ok(NativeIncomingOutcome::MissingDependencies);
        }
        if validated.integrated == 0 && !validated.orphaned {
            return Ok(NativeIncomingOutcome::Applied { changed: false });
        }
        // Validation covered the exact queue and input, so batch apply cannot
        // take its duplicate-sequence refusal after partially mutating.
        self.apply_changes_batch(changes)
            .map_err(|_| NativeMutationError::InvalidOperation)?;
        Ok(NativeIncomingOutcome::Applied {
            changed: validated.integrated != 0,
        })
    }
}
