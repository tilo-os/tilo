//! Strict shared merge: malformed incremental prefixes never mutate a live owner.
use automerge::{AutoCommit, transaction::Transactable};
use collab_types::DocumentMutation;
#[derive(Debug, PartialEq, Eq)]
pub enum MergeError {
    Busy,
    Invalid(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    Changed(bool),
    MissingDependencies,
}

pub fn merge_committed(
    doc: &mut AutoCommit,
    mutation: &DocumentMutation,
) -> Result<bool, MergeError> {
    match merge_committed_with_status(doc, mutation)? {
        MergeOutcome::Changed(changed) => Ok(changed),
        MergeOutcome::MissingDependencies => Ok(false),
    }
}

/// Ceilings for one remote document mutation. They bound parse, semantic
/// validation and batch work; realistic long-lived documents fit well within.
pub fn incoming_document_limits() -> automerge::resource_limits::NativeLimits {
    automerge::resource_limits::NativeLimits {
        max_normalized_bytes: 48 * 1024 * 1024,
        max_operations: 4 * 1024 * 1024,
        max_operation_counter: u32::MAX as u64,
        max_changes: 1024 * 1024,
        max_edges: 16 * 1024 * 1024,
        max_actors: 65_536,
        max_heads: 65_536,
        max_columns: 128,
        max_chunks: 65_536,
        max_value_bytes: 48 * 1024 * 1024,
    }
}
const INCOMING_WORK_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn merge_committed_with_status(
    doc: &mut AutoCommit,
    mutation: &DocumentMutation,
) -> Result<MergeOutcome, MergeError> {
    use automerge::resource_limits::{
        NativeAllowance, NativeDependencyPolicy, NativeIncomingOutcome, NativeMutationError,
    };
    if doc.pending_ops() != 0 {
        return Err(MergeError::Busy);
    }
    let bytes = match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
    };
    let allowance = NativeAllowance::new(INCOMING_WORK_BYTES)
        .map_err(|e| MergeError::Invalid(e.to_string()))?;
    // Inspected, fully validated against this document, then applied; any
    // refusal leaves the live document untouched.
    match doc.apply_incoming_checked(
        bytes,
        &incoming_document_limits(),
        &allowance,
        NativeDependencyPolicy::RequireComplete,
    ) {
        Ok(NativeIncomingOutcome::Applied { changed }) => Ok(MergeOutcome::Changed(changed)),
        Ok(NativeIncomingOutcome::MissingDependencies) => Ok(MergeOutcome::MissingDependencies),
        Err(NativeMutationError::Busy) => Err(MergeError::Busy),
        Err(error) => Err(MergeError::Invalid(error.to_string())),
    }
}

pub(crate) fn snapshot_hash_for(
    bytes: &[u8],
    version: collab_types::DocumentWireVersion,
) -> Result<[u8; 32], collab_types::op::DocumentMutationEncodeError> {
    let mut hash = blake3::Hasher::new();
    DocumentMutation::visit_snapshot_encoding(bytes, version, |part| {
        hash.update(part);
    })?;
    Ok(*hash.finalize().as_bytes())
}

#[cfg(test)]
mod snapshot_hash_tests;
