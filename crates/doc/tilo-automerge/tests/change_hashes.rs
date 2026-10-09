//! `Automerge::change_hashes` lists the change log in the order
//! `get_changes(&[])` does, and adding changes only appends to it
//! (vendor/automerge/TILO-PATCH.md). A history reader relies on both to
//! read only the changes it has not seen.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ChangeHash, ROOT};
use tilo_automerge::ApplyIncoming;

fn logged(doc: &mut AutoCommit) -> Vec<ChangeHash> {
    doc.get_changes(&[])
        .iter()
        .map(automerge::Change::hash)
        .collect()
}

fn edit(doc: &mut AutoCommit, key: &str, value: i64) {
    doc.put(ROOT, key, value).unwrap();
    doc.commit();
}

#[test]
fn change_hashes_list_the_log_in_get_changes_order_and_only_grow() {
    let mut ours = AutoCommit::new().with_actor(ActorId::from(b"ours".as_slice()));
    edit(&mut ours, "a", 1);
    let mut theirs = ours.fork().with_actor(ActorId::from(b"theirs".as_slice()));
    edit(&mut ours, "b", 2);
    edit(&mut theirs, "c", 3);
    edit(&mut theirs, "d", 4);

    let before = ours.committed_document().change_hashes().to_vec();
    assert_eq!(before, logged(&mut ours));

    // Concurrent changes merged in land after everything already there.
    ours.apply_incoming(theirs.get_changes(&[])).unwrap();
    edit(&mut ours, "e", 5);
    let after = ours.committed_document().change_hashes().to_vec();
    assert_eq!(after, logged(&mut ours));
    assert_eq!(after.len(), 5);
    assert_eq!(after.get(..before.len()), Some(before.as_slice()));

    // A loaded copy lists its own log, whatever order that is.
    let mut loaded = AutoCommit::load(&ours.save()).unwrap();
    assert_eq!(
        loaded.committed_document().change_hashes().to_vec(),
        logged(&mut loaded)
    );
}
