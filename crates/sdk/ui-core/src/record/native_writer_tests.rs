//! A device keeps the writer it saved a record under. A change its workspace
//! refuses is taken back from the copy, but the relay still holds the change
//! under that writer, numbered: written on under the same writer after a
//! restart, the record would number its next change the refused one's
//! sequence again, and hold two changes under one writer and sequence — a
//! record no replica reads whole. Taking a change back therefore replaces the
//! saved writer, as a page's claim replaces the writer its saved row carried.
use super::*;
use collab_types::CommitNamed as _;

/// The writer the record is written under now.
fn writer(doc: &AutoCommit) -> automerge::ActorId {
    doc.get_actor().clone()
}

#[test]
fn a_change_taken_back_replaces_the_writer_the_record_saved() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
    let (refused, saved) = {
        let mut owners = RecordOwners::new(root.clone());
        let store = owners.open("so.tilo.test", "workspace").unwrap();
        store.create_record(&checkout).unwrap();
        store
            .with(&checkout.id, |doc| {
                doc.put(automerge::ROOT, "title", "kept").unwrap();
                doc.commit_edit("test.note", "title").unwrap()
            })
            .unwrap();
        // The change the workspace refuses, made under the writer the record
        // saves, and numbered.
        let refused = store
            .with(&checkout.id, |doc| {
                let heads = doc.get_heads();
                doc.put(automerge::ROOT, "body", "refused").unwrap();
                let refused = doc.commit_edit("test.note", "body").unwrap();
                let seq = doc.get_changes(&heads)[0].seq();
                (refused, seq)
            })
            .unwrap();
        let saved = store.read(&checkout.id, writer).unwrap();
        store
            .withdraw_refused(
                &checkout.id,
                &[refused.0],
                "This workspace doesn't allow that.",
            )
            .unwrap()
            .expect("the refused change was withdrawn");
        store.try_flush().unwrap();
        (refused, saved)
    };
    // The device restarts on the same store and opens the record again.
    let mut owners = RecordOwners::new(root.clone());
    let store = owners.open("so.tilo.test", "workspace").unwrap();
    store.try_open_record(&checkout).unwrap();
    assert_eq!(
        store
            .read(&checkout.id, |doc| str_at(doc, &automerge::ROOT, "body"))
            .unwrap(),
        None,
        "the withdrawn change stays withdrawn"
    );
    let after_restart = store.read(&checkout.id, writer).unwrap();
    assert_ne!(
        after_restart, saved,
        "a restart continues the writer taking the change back minted, never the one it left"
    );
    let (seq, written_by, kept) = store
        .with(&checkout.id, |doc| {
            let heads = doc.get_heads();
            doc.put(automerge::ROOT, "title", "written on").unwrap();
            doc.commit_edit("test.note", "title").unwrap();
            let changes = doc.get_changes(&heads);
            let kept = doc
                .get_changes(&[])
                .iter()
                .filter(|change| change.actor_id() == &saved)
                .map(automerge::Change::seq)
                .max();
            (changes[0].seq(), writer(doc), kept)
        })
        .unwrap();
    assert_eq!(
        written_by, after_restart,
        "the change after the refusal is the writer the restart continued"
    );
    assert_ne!(
        written_by, saved,
        "and never the writer the relay holds the refused change under"
    );
    // Nothing under the writer the refusal left is numbered like the refused
    // change again: the number is free here, and the relay's copy of the
    // refused change stands at its own.
    assert_eq!(
        seq, 1,
        "the writer taking the change back starts its own count"
    );
    assert!(
        kept.is_none_or(|highest| highest < refused.1),
        "the record numbers no change under the refused change's writer past it"
    );
    store.try_flush().unwrap();
}
