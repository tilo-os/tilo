//! Strict shared merge: a refused mutation never changes a live owner.
use automerge::{AutoCommit, transaction::Transactable};
use collab_types::DocumentMutation;
#[derive(Debug, PartialEq, Eq)]
pub enum MergeError {
    Busy,
    /// The workspace refused the mutation.
    Invalid(String),
    /// This device could not merge it (its isolated worker crashed, overran
    /// its deadline or never started); the mutation says nothing about it.
    Device(String),
}
impl From<MergeError> for crate::AdmissionError {
    fn from(error: MergeError) -> Self {
        match error {
            // The document this device holds was mid-change: this device's state, which says
            // nothing about the change.
            MergeError::Busy => {
                Self::Device("the shared document has changes of its own open".into())
            }
            MergeError::Invalid(reason) => Self::malformed(reason),
            MergeError::Device(reason) => Self::Device(reason),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    Changed(bool),
    MissingDependencies,
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
        Err(error) => match crate::AdmissionError::from(error) {
            crate::AdmissionError::Rejected { detail, .. } => Err(MergeError::Invalid(detail)),
            crate::AdmissionError::Device(reason) => Err(MergeError::Device(reason)),
            other => Err(MergeError::Invalid(other.to_string())),
        },
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
