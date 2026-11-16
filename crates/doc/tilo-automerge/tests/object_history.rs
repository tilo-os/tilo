//! `latest_changes_within` and `change_writes_within` answer from an object's
//! own ops what reading every change of the document answers.

use automerge::transaction::Transactable as _;
use automerge::{ActorId, AutoCommit, ChangeHash, ObjId, ObjType, ROOT};
use std::collections::{BTreeSet, HashMap};
use tilo_automerge::ApplyIncoming as _;

/// Every object the changes of `doc` show under `top` (deleted ones too),
/// and the newest change of each actor that wrote in any of them: what the
/// fork's methods must agree with, read the slow way.
fn decoded(doc: &mut AutoCommit, top: &ObjId) -> (BTreeSet<String>, BTreeSet<ChangeHash>) {
    let top = top.to_string();
    let mut under: BTreeSet<String> = BTreeSet::from([top.clone()]);
    let mut newest: HashMap<ActorId, (u64, ChangeHash)> = HashMap::new();
    for change in doc.get_changes(&[]) {
        let expanded = change.decode();
        let mut wrote = false;
        for (offset, op) in expanded.operations.iter().enumerate() {
            let id = format!(
                "{}@{}",
                change.start_op().get() + offset as u64,
                change.actor_id()
            );
            let obj = op.obj.to_string();
            if under.contains(&obj) {
                wrote = true;
                if matches!(op.action, automerge::legacy::OpType::Make(_)) {
                    under.insert(id.clone());
                }
            }
            if id == top {
                wrote = true;
            }
        }
        if wrote {
            let seq = change.seq();
            let entry = newest
                .entry(change.actor_id().clone())
                .or_insert_with(|| (seq, change.hash()));
            if seq >= entry.0 {
                *entry = (seq, change.hash());
            }
        }
    }
    (under, newest.into_values().map(|(_, hash)| hash).collect())
}

#[test]
fn what_last_wrote_in_an_object_is_read_from_its_own_ops() {
    let mut ada = AutoCommit::new().with_actor(ActorId::from(b"ada".as_slice()));
    let pages = ada.put_object(ROOT, "pages", ObjType::Map).unwrap();
    let one = ada.put_object(&pages, "one", ObjType::Map).unwrap();
    let two = ada.put_object(&pages, "two", ObjType::Map).unwrap();
    ada.commit();
    let body = ada.put_object(&one, "body", ObjType::Text).unwrap();
    ada.splice_text(&body, 0, 0, "hello").unwrap();
    ada.commit();
    let mut bea = ada.fork().with_actor(ActorId::from(b"bea".as_slice()));
    // Bea writes in note two only; Ada deletes a word of note one, then
    // makes and deletes a block in it.
    bea.put(&two, "title", "Two").unwrap();
    bea.commit();
    ada.splice_text(&body, 0, 2, "").unwrap();
    ada.commit();
    let block = ada.put_object(&one, "block", ObjType::List).unwrap();
    ada.insert(&block, 0, "x").unwrap();
    ada.commit();
    ada.delete(&one, "block").unwrap();
    ada.commit();
    let deleted = ada.get_heads();
    ada.merge_incoming(&mut bea).unwrap();
    // Unrelated writes after it all.
    ada.put(&pages, "three", 3).unwrap();
    ada.commit();

    for object in [&one, &two] {
        let (_, expected) = decoded(&mut ada, object);
        let found: BTreeSet<ChangeHash> = ada
            .committed_document()
            .latest_changes_within(object)
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(found, expected, "{object}");
    }
    assert_eq!(
        ada.committed_document()
            .latest_changes_within(&one)
            .unwrap(),
        deleted,
        "deleting the block is the newest write in note one"
    );

    // Each change, asked of each note, as decoding it says.
    let notes = [one, two];
    let changes = ada.get_changes(&[]);
    for note in &notes {
        let (under, _) = decoded(&mut ada, note);
        for change in &changes {
            let start = change.start_op().get();
            let wrote = change
                .decode()
                .operations
                .iter()
                .enumerate()
                .any(|(offset, op)| {
                    under.contains(&op.obj.to_string())
                        || format!("{}@{}", start + offset as u64, change.actor_id())
                            == note.to_string()
                });
            assert_eq!(
                ada.committed_document()
                    .change_writes_within(&change.hash(), note),
                wrote,
                "change {} in {note}",
                change.hash()
            );
        }
    }
}

#[test]
fn reading_an_objects_writers_reads_only_its_ops() {
    let mut doc = AutoCommit::new();
    let pages = doc.put_object(ROOT, "pages", ObjType::Map).unwrap();
    let note = doc.put_object(&pages, "note", ObjType::Map).unwrap();
    let body = doc.put_object(&note, "body", ObjType::Text).unwrap();
    doc.splice_text(&body, 0, 0, "the note's own text").unwrap();
    doc.commit();
    let read = |doc: &AutoCommit| {
        let before = automerge::work().object_ops;
        doc.committed_document()
            .latest_changes_within(&note)
            .unwrap();
        automerge::work().object_ops - before
    };
    let alone = read(&doc);
    for at in 0..200 {
        let other = doc
            .put_object(&pages, format!("other {at}"), ObjType::Text)
            .unwrap();
        doc.splice_text(&other, 0, 0, &"another note's text ".repeat(20))
            .unwrap();
        doc.commit();
    }
    assert_eq!(read(&doc), alone, "the other notes are not read");
}
