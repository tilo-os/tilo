//! Who made a received change, and the claims of changes read from untrusted bytes.

use collab_types::claim::{
    ChangeClaim, ClaimError, FrameworkChange, UnitPath, claims_of, stamp_invoker,
};
use tilo_actions::ActionContext;

/// Who a change is decided as having been made by, as far as its own message says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Maker {
    /// Its history entry, its stamp or the gesture it continues names who made it.
    Named,
    /// It is a change every holder derives alike, byte for byte, and names no one
    /// ([`collab_types::document::derived::is_derived`]).
    Derived,
    /// A framework change that names no one: whoever signed it made it, the signing device's
    /// person or the agent its key is certified for, and it is decided and recorded as theirs.
    Signer,
    /// It names no one and is no framework change: refused, as every change that says nothing of
    /// who made it.
    Nobody,
}

/// Who `change` is decided as having been made by ([`Maker`]).
#[must_use]
pub fn maker_of(change: &automerge::Change) -> Maker {
    let Some(message) = change.message() else {
        return Maker::Nobody;
    };
    if message.starts_with(tilo_actions::context::CONTINUATION_PREFIX) {
        return Maker::Named;
    }
    // A resource store's change names its invoker in its stamp, which reads as an action
    // context too: one that names no invoker is nobody's, never a named entry's.
    if let Some(stamp) = collab_types::Stamp::of_message(message) {
        return if stamp_invoker(&stamp).is_ok() {
            Maker::Named
        } else {
            Maker::Nobody
        };
    }
    if ActionContext::from_change_message(message).is_some() {
        return Maker::Named;
    }
    if let Some(named) = FrameworkChange::from_change_message(message) {
        return match named {
            FrameworkChange::Genesis | FrameworkChange::Structure
                if collab_types::document::derived::is_derived(change, named, |bytes| {
                    tilo_automerge_isolate::load(bytes).ok()
                }) =>
            {
                Maker::Derived
            }
            // A structure every holder derives is the one kind made apart that must not differ:
            // one that is not the derived change is no structure at all.
            FrameworkChange::Structure => Maker::Nobody,
            _ => Maker::Signer,
        };
    }
    Maker::Nobody
}

/// The changes an incremental document mutation carries, parsed but merged
/// into nothing. The bytes are untrusted, so they are read where every
/// untrusted Automerge input is ([`tilo_automerge_isolate::parse_changes`]).
///
/// # Errors
/// [`ClaimError::Unparsable`] for bytes that are not change chunks.
pub fn parse_changes(bytes: &[u8]) -> Result<Vec<automerge::Change>, ClaimError> {
    tilo_automerge_isolate::parse_changes(bytes)
        .map_err(|error| ClaimError::Unparsable(error.to_string()))
}

/// The claims of `mutation`'s changes, each writing the units `units` lists
/// for it. A snapshot carries none: it is a checkpoint, which only the
/// workspace's pinned governor may publish.
///
/// # Errors
/// The first change that cannot be claimed, or bytes that do not parse.
pub fn mutation_claims(
    mutation: &collab_types::op::DocumentMutation,
    units: impl FnMut(&automerge::Change) -> Vec<UnitPath>,
) -> Result<Vec<ChangeClaim>, ClaimError> {
    match mutation {
        collab_types::op::DocumentMutation::Snapshot(_) => Ok(Vec::new()),
        collab_types::op::DocumentMutation::Incremental(bytes) => {
            claims_of(&parse_changes(bytes)?, units)
        }
    }
}

#[cfg(test)]
mod tests {
    use automerge::transaction::Transactable as _;
    use collab_types::claim::CommitNamed as _;
    use collab_types::{DocumentMutation, record_unit};

    use super::*;

    tilo_test_env::scenario! {
        /// How the changes of untrusted bytes are read and claimed.
        fn changes_read_from_bytes_are_claimed;

        phase a_mutation_claims_each_change_and_a_snapshot_none;
        {
            let mut doc = automerge::AutoCommit::new();
            let before = doc.get_heads();
            for field in ["favorite", "title"] {
                doc.put(automerge::ROOT, field, true).unwrap();
                doc.commit_edit("note", field, &[1; 32]);
            }
            let mutation = DocumentMutation::Incremental(doc.save_after(&before));
            let claims = mutation_claims(&mutation, record_unit).unwrap();
            let made: Vec<[u8; 32]> = doc
                .get_changes(&before)
                .iter()
                .map(|change| change.hash().0)
                .collect();
            assert_eq!(
                claims.iter().map(|claim| claim.change).collect::<Vec<_>>(),
                made,
                "one claim per change, in order"
            );
            assert!(claims.iter().all(|claim| claim.units == [Vec::new()]));
            let snapshot = DocumentMutation::Snapshot(doc.save());
            assert!(mutation_claims(&snapshot, record_unit).unwrap().is_empty());
        }

        phase changes_parse_without_their_dependencies;
        {
            let mut doc = automerge::AutoCommit::new();
            doc.put(automerge::ROOT, "title", "a").unwrap();
            doc.commit();
            let before = doc.get_heads();
            doc.put(automerge::ROOT, "title", "b").unwrap();
            doc.commit();
            let changes = parse_changes(&doc.save_after(&before)).unwrap();
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].deps(), before.as_slice());
        }
    }
}
