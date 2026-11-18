//! A document hands back the change it last committed as the commit built it,
//! and names where a change writes from its operations' objects and keys alone
//! (vendor/automerge/TILO-PATCH.md). Readers that follow a write (a view, the
//! check of what a caller may write) rely on both being what rebuilding and
//! decoding the change would give.

use std::collections::HashSet;

use automerge::legacy::{Key, ObjectId, OpType};
use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, Change, ObjId, ObjType, ROOT, ReadDoc};
use tilo_automerge::ApplyIncoming;

/// Where `change` writes, read by decoding every operation.
fn decoded(doc: &AutoCommit, change: &Change) -> (Vec<(ObjId, Option<String>)>, usize) {
    let expanded = change.decode();
    let object = |obj: &ObjectId| match obj {
        ObjectId::Root => ROOT,
        ObjectId::Id(id) => doc.import(&id.to_string()).unwrap().0,
    };
    let mut seen = HashSet::new();
    let mut writes = Vec::new();
    let mut made = 0;
    for op in &expanded.operations {
        let key = match &op.key {
            Key::Map(key) => Some(key.to_string()),
            Key::Seq(_) => None,
        };
        let at = (object(&op.obj), key);
        if seen.insert(at.clone()) {
            writes.push(at);
        }
        if matches!(op.action, OpType::Make(_)) {
            made += 1;
        }
    }
    (writes, made)
}

#[test]
fn a_change_names_where_it_writes_and_the_last_local_one_comes_back_as_built() {
    let mut doc = AutoCommit::new().with_actor(ActorId::from(b"ours".as_slice()));
    let items = doc.put_object(ROOT, "items", ObjType::Map).unwrap();
    doc.put(ROOT, "title", "list").unwrap();
    doc.commit();
    let before = doc.get_heads();
    for index in 0..20 {
        let item = doc
            .put_object(&items, format!("item-{index}"), ObjType::Map)
            .unwrap();
        doc.put(&item, "done", false).unwrap();
        let text = doc.put_object(&item, "text", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "some words").unwrap();
    }
    doc.delete(ROOT, "title").unwrap();
    doc.commit();
    let head = doc.get_heads()[0];

    // As the commit built it, and as a document without it builds it again.
    let kept = doc.get_change_by_hash(&head).unwrap();
    let rebuilt = AutoCommit::load(&doc.save())
        .unwrap()
        .get_change_by_hash(&head)
        .unwrap();
    assert_eq!(kept.raw_bytes(), rebuilt.raw_bytes());
    let since = doc.get_changes(&before);
    assert_eq!(since.len(), 1);
    assert_eq!(since[0].raw_bytes(), rebuilt.raw_bytes());

    let touches = doc.committed_document().change_touches(&kept).unwrap();
    let (writes, made) = decoded(&doc, &kept);
    assert_eq!(touches.writes, writes);
    assert_eq!(touches.made.len(), made);
    for (object, parent, key) in &touches.made {
        let parents: Vec<_> = doc.parents(object).unwrap().collect();
        let first = parents.first().unwrap();
        assert_eq!(&first.obj, parent);
        let at = match &first.prop {
            automerge::Prop::Map(key) => Some(key.clone()),
            automerge::Prop::Seq(_) => None,
        };
        assert_eq!(&at, key);
    }

    // A change another actor made, applied here, names the same as decoding it.
    let mut theirs = doc.fork().with_actor(ActorId::from(b"theirs".as_slice()));
    let item = theirs.get(&items, "item-3").unwrap().unwrap().1;
    theirs.put(&item, "done", true).unwrap();
    theirs.put_object(&item, "tags", ObjType::List).unwrap();
    theirs.commit();
    let theirs_head = theirs.get_heads()[0];
    doc.apply_incoming(vec![theirs.get_change_by_hash(&theirs_head).unwrap()])
        .unwrap();
    let applied = doc.get_change_by_hash(&theirs_head).unwrap();
    let touches = doc.committed_document().change_touches(&applied).unwrap();
    assert_eq!(touches.writes, decoded(&doc, &applied).0);
    assert_eq!(touches.made.len(), 1);
    // The last local change still comes back, now that it is not the head.
    assert_eq!(
        doc.get_change_by_hash(&head).unwrap().raw_bytes(),
        rebuilt.raw_bytes()
    );
    assert_eq!(doc.get_changes(&before).len(), 2);

    // A document that does not hold the change knows none of its actors' objects.
    let stranger = AutoCommit::new();
    assert!(
        stranger
            .committed_document()
            .change_touches(&applied)
            .is_none()
    );
}
