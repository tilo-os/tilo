//! Strict shared merge: a refused mutation never changes a live owner.
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

/// Merge a committed mutation into `doc` through isolated admission.
pub fn merge_committed(
    doc: &mut AutoCommit,
    mutation: &DocumentMutation,
) -> Result<bool, MergeError> {
    match merge_committed_with_status(doc, mutation)? {
        MergeOutcome::Changed(changed) => Ok(changed),
        MergeOutcome::MissingDependencies => Ok(false),
    }
}

pub(crate) fn merge_committed_with_status(
    doc: &mut AutoCommit,
    mutation: &DocumentMutation,
) -> Result<MergeOutcome, MergeError> {
    use tilo_automerge_isolate::{Applied, Dependencies};
    if doc.pending_ops() != 0 {
        return Err(MergeError::Busy);
    }
    let bytes = match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
    };
    // Decoded and applied in an isolated worker first; any refusal leaves the
    // live document untouched.
    match tilo_automerge_isolate::apply(doc, bytes, Dependencies::RequireComplete) {
        Ok(Applied::Changed(changed)) => Ok(MergeOutcome::Changed(changed)),
        Ok(Applied::MissingDependencies) => Ok(MergeOutcome::MissingDependencies),
        Err(error) => Err(MergeError::Invalid(error.to_string())),
    }
}

pub(crate) fn snapshot_hash(
    bytes: &[u8],
) -> Result<[u8; 32], collab_types::op::DocumentMutationEncodeError> {
    let mut hash = blake3::Hasher::new();
    DocumentMutation::visit_snapshot_encoding(bytes, |part| {
        hash.update(part);
    })?;
    Ok(*hash.finalize().as_bytes())
}

#[cfg(test)]
mod snapshot_hash_tests;
