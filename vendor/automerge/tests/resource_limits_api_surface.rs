//! Compile-level contract of `automerge::resource_limits`.
//!
//! Names every public item the Cloud crates rely on with its exact
//! signature (as fn-pointer coercions where the item is not generic, and as
//! typed calls inside a never-run function where it is). A removed, renamed
//! or re-typed item fails to compile here, before any Cloud crate notices.
#![allow(dead_code, unused_variables, unreachable_code, clippy::type_complexity)]

use automerge::resource_limits::{
    CheckedStorage, NativeAllocationSet, NativeAllowance, NativeApplyResult, NativeChanges, NativeCloneCandidate,
    NativeDependencyPolicy, NativeFootprint, NativeHashes, NativeHostCredit, NativeId, NativeIncomingOutcome,
    NativeLimits, NativeListEntry, NativeMutationError, NativeObject, NativeOwned, NativeReadError,
    NativeReadSession, NativeReadSteps, NativeReservation, NativeResourceError, NativeRetention, NativeSaved,
    NativeScalar, NativeSelection, NativeShape, NativeValue, NativeWorkTurn, NormalizedStorage,
};
use automerge::{AutoCommit, ChangeHash, ObjType, SaveOptions};
use std::ops::ControlFlow;

type R<T> = Result<T, NativeResourceError>;

#[test]
fn ledger_and_limits_surface() {
    // NativeLimits: every field, public and typed.
    let NativeLimits {
        max_normalized_bytes: _,
        max_operations: _,
        max_operation_counter: _,
        max_changes: _,
        max_edges: _,
        max_actors: _,
        max_heads: _,
        max_columns: _,
        max_chunks: _,
        max_value_bytes: _,
    } = NativeLimits::default();
    let l = NativeLimits::default();
    let _: (usize, usize, u64, usize, usize, usize, usize, usize, usize, usize) = (
        l.max_normalized_bytes,
        l.max_operations,
        l.max_operation_counter,
        l.max_changes,
        l.max_edges,
        l.max_actors,
        l.max_heads,
        l.max_columns,
        l.max_chunks,
        l.max_value_bytes,
    );
    // The 0.8 defaults are part of the contract (callers size host pools from them).
    assert_eq!(l.max_normalized_bytes, 8 * 1024 * 1024);
    assert_eq!(l.max_operations, 65_536);
    assert_eq!(l.max_operation_counter, 65_536);
    assert_eq!(l.max_changes, 65_536);
    assert_eq!(l.max_edges, 262_144);
    assert_eq!(l.max_actors, 1_024);
    assert_eq!(l.max_heads, 4_096);
    assert_eq!(l.max_columns, 128);
    assert_eq!(l.max_chunks, 1_024);
    assert_eq!(l.max_value_bytes, 8 * 1024 * 1024);
    let _: fn(&NativeLimits) -> NativeLimits = NativeLimits::clone;

    // NativeResourceError: closed set of variants, PartialEq + Error.
    fn resource_variants(e: NativeResourceError) -> u8 {
        match e {
            NativeResourceError::Unaccounted => 0,
            NativeResourceError::NormalizedBytes => 1,
            NativeResourceError::Operations => 2,
            NativeResourceError::Collection => 3,
            NativeResourceError::ValueBytes => 4,
            NativeResourceError::WorkBytes => 5,
            NativeResourceError::Invalid => 6,
        }
    }
    fn is_error<E: std::error::Error + Eq + Send + Sync + 'static>() {}
    is_error::<NativeResourceError>();

    // NativeAllowance / NativeReservation / NativeHostCredit.
    let _: fn(usize) -> R<NativeAllowance> = NativeAllowance::new;
    let _: fn(usize, Box<dyn NativeHostCredit>) -> R<NativeAllowance> = NativeAllowance::with_host_credit;
    let _: fn(&NativeAllowance, usize) -> R<NativeReservation> = NativeAllowance::reserve;
    let _: fn(&NativeAllowance) -> usize = NativeAllowance::available_bytes;
    let _: fn(&NativeAllowance) = NativeAllowance::finish_turn;
    let _: fn(&NativeAllowance) -> usize = NativeAllowance::retained_bytes;
    let _: fn(&NativeAllowance) -> NativeAllowance = NativeAllowance::clone;
    let _: fn(&NativeReservation) -> usize = NativeReservation::bytes;
    struct Credit;
    impl NativeHostCredit for Credit {
        fn shrink_to(&self, native_retained_bytes: usize) {}
        fn release(self: Box<Self>) {}
    }
    fn send_sync<T: Send + Sync>() {}
    send_sync::<NativeAllowance>();
    send_sync::<NativeReservation>();

    // Inspection / normalization.
    let _: for<'a> fn(&'a [u8], &NativeLimits, &mut NativeAllowance) -> R<CheckedStorage<'a>> =
        AutoCommit::inspect_bounded;
    storage_surface();
    let _: fn(&NativeShape) -> usize = NativeShape::operations;
    let _: fn(&NativeShape) -> usize = NativeShape::normalized_bytes;
    let _: fn(&NativeShape) -> usize = NativeShape::dense_operations;
    let _: fn(&NativeShape) -> u64 = NativeShape::maximum_operation_counter;
}

/// Methods of lifetime-carrying storage types, for one input lifetime `'s`
/// (the impl lifetime is early-bound, so it cannot be a `for<>` binder).
fn storage_surface<'s>() {
    let _: for<'b> fn(&'b CheckedStorage<'s>) -> &'b [u8] = CheckedStorage::original_bytes;
    let _: for<'b> fn(&'b CheckedStorage<'s>) -> &'b NativeShape = CheckedStorage::shape;
    let _: fn(CheckedStorage<'s>, &NativeAllowance) -> R<NormalizedStorage<'s>> = CheckedStorage::normalize;
    let _: for<'b> fn(&'b NormalizedStorage<'s>) -> &'b [u8] = NormalizedStorage::bytes;
    let _: for<'b> fn(&'b NormalizedStorage<'s>) -> &'b [u8] = NormalizedStorage::original_bytes;
    let _: for<'b> fn(&'b NormalizedStorage<'s>) -> &'b NativeShape = NormalizedStorage::shape;
    // Load.
    let _: fn(NormalizedStorage<'s>, &mut NativeAllocationSet, &NativeWorkTurn) -> R<NativeOwned<AutoCommit>> =
        NormalizedStorage::reconstruct;
}

#[test]
fn custody_surface() {
    let _: fn(NativeAllowance) -> R<NativeAllocationSet> = NativeAllocationSet::new;
    let _: fn(&NativeAllocationSet, NativeAllowance) -> R<NativeWorkTurn> = NativeAllocationSet::work_turn;
    let _: fn(&mut NativeAllocationSet) = NativeAllocationSet::reconcile;
    let _: for<'a> fn(&NativeWorkTurn, &'a [u8], &NativeLimits) -> R<NormalizedStorage<'a>> =
        NativeWorkTurn::normalize_input;
    let _: fn(&NativeWorkTurn, usize) -> R<NativeReservation> = NativeWorkTurn::reserve;

    let _: fn(&AutoCommit, &NativeAllowance) -> R<NativeFootprint> = AutoCommit::native_footprint;
    let _: fn(&AutoCommit, &mut NativeAllocationSet, &NativeWorkTurn) -> R<NativeCloneCandidate<AutoCommit>> =
        AutoCommit::clone_bounded;
    let _: fn(&NativeFootprint) -> usize = NativeFootprint::owned_bytes;
    let _: fn(&NativeFootprint) -> usize = NativeFootprint::shared_bytes;
    let _: fn(&NativeFootprint) -> usize = NativeFootprint::total_bytes;

    let _: fn(&NativeOwned<AutoCommit>) -> usize = NativeOwned::<AutoCommit>::pending_ops;
    let _: fn(&NativeOwned<AutoCommit>, &mut NativeAllocationSet, &NativeWorkTurn) -> R<NativeOwned<AutoCommit>> =
        NativeOwned::<AutoCommit>::clone_bounded;
    let _: fn(&NativeCloneCandidate<AutoCommit>) -> usize = NativeCloneCandidate::<AutoCommit>::pending_ops;
    let _: fn(
        &NativeCloneCandidate<AutoCommit>,
        &mut NativeAllocationSet,
        &NativeWorkTurn,
    ) -> R<NativeCloneCandidate<AutoCommit>> = NativeCloneCandidate::<AutoCommit>::clone_bounded;

    // Load.

    // Save / history.
    let _: fn(&NativeOwned<AutoCommit>, &NativeWorkTurn, SaveOptions, usize) -> R<NativeSaved> =
        NativeOwned::<AutoCommit>::save_bounded_with_limit;
    let _: fn(&NativeOwned<AutoCommit>, &NativeWorkTurn, SaveOptions) -> R<NativeSaved> =
        NativeOwned::<AutoCommit>::save_bounded;
    let _: fn(&NativeCloneCandidate<AutoCommit>, &NativeWorkTurn, SaveOptions, usize) -> R<NativeSaved> =
        NativeCloneCandidate::<AutoCommit>::save_bounded_with_limit;
    let _: fn(&NativeCloneCandidate<AutoCommit>, &NativeWorkTurn, SaveOptions) -> R<NativeSaved> =
        NativeCloneCandidate::<AutoCommit>::save_bounded;
    let _: fn(&NativeSaved) -> &[u8] = NativeSaved::bytes;
    let _: fn(&NativeOwned<AutoCommit>, &[ChangeHash], &NativeWorkTurn) -> R<NativeChanges> =
        NativeOwned::<AutoCommit>::changes_since_bounded;
    let _: fn(&NativeOwned<AutoCommit>, &NativeWorkTurn) -> R<NativeHashes> = NativeOwned::<AutoCommit>::base_heads_bounded;
    let _: fn(&NativeOwned<AutoCommit>) -> bool = NativeOwned::<AutoCommit>::has_missing_dependencies;
    let _: fn(&NativeHashes) -> &[ChangeHash] = NativeHashes::as_slice;
    let _: fn(&NativeChanges) -> usize = NativeChanges::len;
    let _: fn(&NativeChanges) -> bool = NativeChanges::is_empty;
    let _: fn(&NativeChanges, usize) -> Option<&[u8]> = NativeChanges::raw_bytes;
    let _: fn(&NativeChanges, usize) -> Option<ChangeHash> = NativeChanges::hash;

    // Debug is implemented for every public custody type (callers log them).
    fn debug<T: std::fmt::Debug>() {}
    debug::<NativeAllocationSet>();
    debug::<NativeWorkTurn>();
    debug::<NativeOwned<AutoCommit>>();
    debug::<NativeCloneCandidate<AutoCommit>>();
    debug::<NativeRetention>();
    debug::<NativeFootprint>();
    debug::<NativeSaved>();
    debug::<NativeHashes>();
    debug::<NativeChanges>();
    debug::<NativeApplyResult>();
    debug::<NativeShape>();
    debug::<CheckedStorage<'static>>();
    debug::<NormalizedStorage<'static>>();
    debug::<NativeReadSteps>();
    // Custody moves across threads with its document.
    fn send<T: Send>() {}
    send::<NativeOwned<AutoCommit>>();
    send::<NativeAllocationSet>();
    send::<NativeWorkTurn>();
}

#[test]
fn apply_surface() {
    let _: fn(
        NativeOwned<AutoCommit>,
        NormalizedStorage<'_>,
        NativeDependencyPolicy,
        &mut NativeAllocationSet,
        &NativeWorkTurn,
    ) -> Result<NativeApplyResult, NativeMutationError> = NativeOwned::<AutoCommit>::apply_checked_bounded;
    let _: fn(
        &mut AutoCommit,
        &[u8],
        &NativeLimits,
        &NativeAllowance,
        NativeDependencyPolicy,
    ) -> Result<NativeIncomingOutcome, NativeMutationError> = AutoCommit::apply_incoming_checked;

    fn policy(p: NativeDependencyPolicy) -> u8 {
        match p {
            NativeDependencyPolicy::RequireComplete => 0,
            NativeDependencyPolicy::RetainOrphans => 1,
        }
    }
    fn mutation(e: NativeMutationError) -> Option<NativeResourceError> {
        match e {
            NativeMutationError::Resource(r) => Some(r),
            NativeMutationError::Busy
            | NativeMutationError::InvalidTarget
            | NativeMutationError::InvalidOperation
            | NativeMutationError::DuplicateSequence => None,
        }
    }
    let _: NativeMutationError = NativeResourceError::WorkBytes.into();
    fn result(r: NativeApplyResult) -> Option<(NativeOwned<AutoCommit>, bool, bool)> {
        match r {
            NativeApplyResult::Applied { document, changed, missing_dependencies } => {
                Some((document, changed, missing_dependencies))
            }
            NativeApplyResult::MissingDependencies => None,
        }
    }
    fn outcome(o: NativeIncomingOutcome) -> Option<bool> {
        match o {
            NativeIncomingOutcome::Applied { changed } => Some(changed),
            NativeIncomingOutcome::MissingDependencies => None,
        }
    }
    fn traits<T: Copy + Eq + std::fmt::Debug>() {}
    traits::<NativeDependencyPolicy>();
    traits::<NativeIncomingOutcome>();
    fn error<E: std::error::Error + Send + Sync + 'static>() {}
    error::<NativeMutationError>();
}

/// Generic read API: typed calls in a function that is compiled, never run.
#[allow(clippy::diverging_sub_expression)]
fn read_surface(owned: &NativeOwned<AutoCommit>, candidate: &NativeCloneCandidate<AutoCommit>, turn: &NativeWorkTurn) {
    let read: NativeReadSession<'_, '_> = owned.read(turn).unwrap();
    let _: NativeReadSession<'_, '_> = candidate.read_unqualified(turn).unwrap();
    let _: R<NativeReservation> = read.reserve_output(1);
    let root: NativeObject<'_> = read.root();
    let _: NativeId<'_> = root.id();
    let _: usize = read.pending_ops();
    let _: usize = read.operation_count();
    let steps: NativeReadSteps = NativeReadSteps::new(10).unwrap();
    let _: R<()> = steps.charge(1);
    let _: usize = steps.remaining();

    let _: Result<usize, NativeReadError<()>> =
        read.visit_property(root, "k", NativeSelection::Winner, |value: NativeValue<'_, '_>, id: NativeId<'_>| Ok(()));
    let _: Result<usize, NativeReadError<()>> =
        read.visit_property_with_steps(root, "k", NativeSelection::All, &steps, |value, id| Ok(()));
    let _: Result<usize, NativeReadError<()>> = read.visit_keys(root, |key: &str| Ok(()));
    let _: Result<usize, NativeReadError<()>> =
        read.visit_keys_with_steps(root, &steps, |key: &str| Ok(ControlFlow::Continue(())));
    let _: Result<usize, NativeReadError<()>> = read.visit_list(root, &steps, |entry: NativeListEntry<'_, '_>| {
        let NativeListEntry { index, value, id, is_mark, conflict } = entry;
        let _: (usize, NativeValue<'_, '_>, NativeId<'_>, bool, bool) = (index, value, id, is_mark, conflict);
        Ok(ControlFlow::Break(()))
    });
    let _: R<usize> = read.visit_heads(4, |hash: ChangeHash| {});
    let id = root.id();
    let mut sink = [0u8; 64];
    let _: R<usize> = read.id_len(id);
    let _: R<usize> = read.write_id(id, &mut sink);
    let _: R<usize> = read.id_bytes_len(id);
    let _: R<usize> = read.write_id_bytes(id, &mut sink);
    let _: R<Option<u64>> = read.visit_id_parts(id, |parts: Option<(u64, &[u8])>| parts.map(|(counter, actor)| counter));

    // Value shapes.
    fn value(v: NativeValue<'_, '_>) {
        match v {
            NativeValue::Object { kind, object } => {
                let _: (ObjType, NativeObject<'_>) = (kind, object);
            }
            NativeValue::Scalar(s) => scalar(s),
        }
    }
    fn scalar(s: NativeScalar<'_>) {
        let _: () = match s {
            NativeScalar::Bytes(b) => {
                let _: &[u8] = b;
            }
            NativeScalar::Str(s) => {
                let _: &str = s;
            }
            NativeScalar::Int(i) | NativeScalar::Counter(i) | NativeScalar::Timestamp(i) => {
                let _: i64 = i;
            }
            NativeScalar::Uint(u) => {
                let _: u64 = u;
            }
            NativeScalar::F64(f) => {
                let _: f64 = f;
            }
            NativeScalar::Boolean(b) => {
                let _: bool = b;
            }
            NativeScalar::Unknown { type_code, bytes } => {
                let _: (u8, &[u8]) = (type_code, bytes);
            }
            NativeScalar::Null => {}
        };
        let mut out = String::new();
        let _: std::fmt::Result = s.write_legacy_debug(&mut out);
    }
    fn read_error(e: NativeReadError<u8>) {
        match e {
            NativeReadError::Resource(r) => {
                let _: NativeResourceError = r;
            }
            NativeReadError::Visitor(v) => {
                let _: u8 = v;
            }
        }
    }
    let _: NativeReadError<u8> = NativeResourceError::Operations.into();
    fn traits<T: Copy + Eq + std::fmt::Debug>() {}
    traits::<NativeSelection>();
    traits::<NativeId<'static>>();
    traits::<NativeObject<'static>>();
    fn copy_partial<T: Copy + PartialEq + std::fmt::Debug>() {}
    copy_partial::<NativeScalar<'static>>();
    copy_partial::<NativeValue<'static, 'static>>();
    copy_partial::<NativeListEntry<'static, 'static>>();
}

#[test]
fn read_surface_compiles() {
    let _: fn(&NativeOwned<AutoCommit>, &NativeCloneCandidate<AutoCommit>, &NativeWorkTurn) = read_surface;
}

#[test]
fn read_steps_bounds_are_the_08_contract() {
    assert_eq!(NativeReadSteps::new(0).unwrap_err(), NativeResourceError::Operations);
    assert_eq!(NativeReadSteps::new(1_000_001).unwrap_err(), NativeResourceError::Operations);
    let steps = NativeReadSteps::new(1_000_000).unwrap();
    steps.charge(999_999).unwrap();
    assert_eq!(steps.remaining(), 1);
    assert_eq!(steps.charge(2), Err(NativeResourceError::Operations));
    assert_eq!(steps.remaining(), 1, "a refused charge spends nothing");
}
