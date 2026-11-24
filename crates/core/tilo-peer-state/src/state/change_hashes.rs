//! The changes a document mutation's payload carries: what a device inside a workspace
//! checks a write's dependencies against.

use collab_types::DocumentMutation;

/// The hashes of the changes `payload`, one document mutation, carries.
pub(crate) fn change_hashes(payload: &[u8]) -> Vec<[u8; 32]> {
    let Ok(mutation) = DocumentMutation::decode(payload) else {
        return Vec::new();
    };
    let bytes = match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
    };
    tilo_failure::ignored!(
        "a payload that is not automerge bytes names no changes this device lacks",
        collab_admission::parse_changes(&bytes).unwrap_or_default()
    )
    .iter()
    .map(automerge::Change::hash)
    .map(|hash| hash.0)
    .collect()
}
