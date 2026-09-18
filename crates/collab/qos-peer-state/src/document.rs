//! Strict shared merge: malformed incremental prefixes never mutate a live owner.
use automerge::{AutoCommit, transaction::Transactable};
use collab_types::DocumentMutation;
#[derive(Debug, PartialEq, Eq)]
pub enum MergeError {
    Busy,
    Invalid(String),
}
pub fn merge_committed(
    doc: &mut AutoCommit,
    mutation: &DocumentMutation,
) -> Result<bool, MergeError> {
    if doc.pending_ops() != 0 {
        return Err(MergeError::Busy);
    }
    let mut incoming = match mutation {
        DocumentMutation::Snapshot(bytes) => {
            AutoCommit::load(bytes).map_err(|e| MergeError::Invalid(e.to_string()))?
        }
        DocumentMutation::Incremental(bytes) => {
            match AutoCommit::load(bytes) {
                Ok(_) | Err(automerge::AutomergeError::MissingDeps) => {}
                Err(error) => return Err(MergeError::Invalid(error.to_string())),
            }
            let mut incoming = doc.clone();
            incoming
                .load_incremental(bytes)
                .map_err(|e| MergeError::Invalid(e.to_string()))?;
            incoming
        }
    };
    let before = doc.base_heads();
    let mut candidate = doc.clone();
    candidate
        .merge(&mut incoming)
        .map_err(|e| MergeError::Invalid(e.to_string()))?;
    let changed = before != candidate.base_heads();
    *doc = candidate;
    Ok(changed)
}
