//! Taking another device's copy of a record into this device's.
//!
//! Every record the desk keeps is one automerge document per record, written
//! by every device in the workspace: a device takes what the others wrote
//! into its own copy, and what merged in is what its next read sees. The
//! merge happens here so that the reading a caller holds of its own document
//! survives it — a shared desktop's writer, a person's arrangement and a
//! session's desk all take a record in the same way.

use automerge::AutoCommit;

/// Take in `other`, another device's copy of the record: how many changes
/// merged in besides what `doc` already held.
///
/// # Errors
/// Why `other`'s changes cannot be taken in; `doc` is unchanged.
pub fn merge_into(
    doc: &mut AutoCommit,
    other: &AutoCommit,
) -> Result<usize, automerge::AutomergeError> {
    // Merging takes the other document mutably; a copy is merged so the
    // caller's own reading stands as it was.
    let mut other = other.clone();
    tilo_automerge::ApplyIncoming::merge_incoming(doc, &mut other).map(|added| added.len())
}
