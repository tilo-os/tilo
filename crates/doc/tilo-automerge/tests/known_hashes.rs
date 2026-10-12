//! Loading a document with the change hashes its saver listed
//! (`LoadOptions::known_change_hashes`, vendor/automerge/TILO-PATCH.md)
//! builds the same document as rebuilding and hashing every change: the same
//! log, heads, op counts, authors and saved bytes, and it syncs the same way
//! afterwards. Hashes that cannot stand for the document's changes are
//! ignored and the changes rebuilt.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, Author, AutoCommit, ChangeHash, LoadOptions, ObjType, ROOT, ScalarValue};
use tilo_automerge::ApplyIncoming;

fn actor(name: &str) -> ActorId {
    ActorId::from(name.as_bytes())
}

/// Two collaborators editing one record concurrently, plus a third who
/// started an unrelated document and merged it in: maps with overwrites
/// and deletes, conflicting writes, a counter and its increments, text with
/// deletions and marks, authors, messages and empty changes. Changes with
/// no dependencies: each actor's first, one of them empty.
fn collaborated() -> AutoCommit {
    let mut ada = AutoCommit::new()
        .with_actor(actor("ada"))
        .with_author(Some(Author::from(b"ada@device".to_vec())));
    let tasks = ada.put_object(ROOT, "tasks", ObjType::Map).unwrap();
    let title = ada.put_object(&tasks, "title", ObjType::Text).unwrap();
    ada.splice_text(&title, 0, 0, "the quick brown fox")
        .unwrap();
    ada.put(&ROOT, "count", ScalarValue::counter(1)).unwrap();
    ada.commit_with(CommitOptions::default().with_message("start"));
    for i in 0..40 {
        ada.put(&tasks, format!("k{}", i % 7), i).unwrap();
        ada.increment(&ROOT, "count", 2).unwrap();
        ada.commit();
    }

    let mut bo = ada.fork().with_actor(actor("bo"));
    bo.set_author(Some(Author::from(b"bo@device".to_vec())));
    bo.splice_text(&title, 4, 6, "slow").unwrap();
    bo.mark(
        &title,
        Mark::new("bold".into(), true, 0, 3),
        ExpandMark::After,
    )
    .unwrap();
    bo.delete(&tasks, "k3").unwrap();
    bo.put(&tasks, "k1", "bo").unwrap();
    bo.commit();
    bo.empty_change(CommitOptions::default().with_message("nothing"));

    ada.put(&tasks, "k1", "ada").unwrap();
    ada.splice_text(&title, 0, 3, "a").unwrap();
    ada.increment(&ROOT, "count", 5).unwrap();
    ada.commit();

    let mut cy = AutoCommit::new().with_actor(actor("cy"));
    cy.empty_change(CommitOptions::default());
    cy.put(&ROOT, "cy", "independent").unwrap();
    cy.commit();
    let mut dee = AutoCommit::new().with_actor(actor("dee"));
    let list = dee.put_object(ROOT, "list", ObjType::List).unwrap();
    for i in 0..10 {
        dee.insert(&list, i, i64::try_from(i).unwrap()).unwrap();
    }
    dee.delete(&list, 4).unwrap();
    dee.commit();

    ada.merge_incoming(&mut bo).unwrap();
    ada.merge_incoming(&mut cy).unwrap();
    ada.merge_incoming(&mut dee).unwrap();
    ada.put(&tasks, "after", "merge").unwrap();
    ada.commit();
    ada
}

/// A long single-actor history, one change per item, like a list filled
/// item by item.
fn appended(items: usize) -> AutoCommit {
    let mut doc = AutoCommit::new().with_actor(actor("list"));
    let map = doc.put_object(ROOT, "items", ObjType::Map).unwrap();
    doc.commit();
    for i in 0..items {
        let entry = doc
            .put_object(&map, format!("id{i:05}"), ObjType::Map)
            .unwrap();
        let text = doc.put_object(&entry, "title", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, &format!("item {i}")).unwrap();
        doc.put(&entry, "done", i % 3 == 0).unwrap();
        doc.commit();
    }
    doc
}

fn hashes_of(doc: &AutoCommit) -> Vec<ChangeHash> {
    doc.committed_document().change_hashes().to_vec()
}

fn load_known(bytes: &[u8], hashes: &[ChangeHash]) -> AutoCommit {
    AutoCommit::load_with_options(bytes, LoadOptions::new().known_change_hashes(hashes)).unwrap()
}

/// Everything history, sync and attribution read from a document's log.
fn assert_same_log(known: &mut AutoCommit, full: &mut AutoCommit) {
    assert_eq!(hashes_of(known), hashes_of(full));
    assert_eq!(known.get_heads(), full.get_heads());
    let rebuilt = |doc: &mut AutoCommit| {
        doc.get_changes(&[])
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect::<Vec<_>>()
    };
    assert_eq!(rebuilt(known), rebuilt(full));
    let meta = |doc: &mut AutoCommit| {
        doc.get_changes_meta(&[])
            .into_iter()
            .map(|meta| {
                (
                    meta.hash,
                    meta.actor.into_owned(),
                    meta.author.map(|author| author.as_bytes().to_vec()),
                    meta.seq,
                    meta.start_op,
                    meta.max_op,
                    meta.deps,
                    meta.message.map(std::borrow::Cow::into_owned),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(meta(known), meta(full));
    assert_eq!(known.save(), full.save());
}

#[test]
fn a_load_with_known_hashes_builds_the_document_a_full_load_builds() {
    for mut source in [collaborated(), appended(300)] {
        let bytes = source.save();
        let hashes = hashes_of(&source);
        let mut full = AutoCommit::load(&bytes).unwrap();
        let mut known = load_known(&bytes, &hashes);
        assert_same_log(&mut known, &mut full);

        // The hashes were taken, not rebuilt: two early changes listed in
        // swapped order (heads unchanged) load in that order.
        let mut swapped = hashes.clone();
        swapped.swap(0, 1);
        assert_eq!(hashes_of(&load_known(&bytes, &swapped)), swapped);

        // Both continue the same way: a collaborator's next change merges
        // into each, and each hands its own next change over the same.
        let mut peer = source.fork().with_actor(actor("peer"));
        peer.put(ROOT, "next", "from a peer").unwrap();
        peer.commit();
        let incoming = peer.save_after(&source.get_heads());
        for doc in [&mut known, &mut full] {
            doc.set_actor(actor("me"));
            doc.load_incoming(&incoming).unwrap();
            doc.put(ROOT, "mine", 1).unwrap();
            doc.commit_with(CommitOptions::default().with_time(1));
        }
        let heads = peer.get_heads();
        assert_eq!(known.save_after(&heads), full.save_after(&heads));
        assert_same_log(&mut known, &mut full);
    }
}

#[test]
fn appended_chunks_after_the_hinted_document_still_load() {
    let mut doc = appended(50);
    let mut bytes = doc.save();
    let hashes = hashes_of(&doc);
    let sent = doc.get_heads();
    doc.put(ROOT, "later", "appended").unwrap();
    doc.commit();
    bytes.extend(doc.save_after(&sent));

    let mut full = AutoCommit::load(&bytes).unwrap();
    let mut known = load_known(&bytes, &hashes);
    assert_same_log(&mut known, &mut full);
}

#[test]
fn hashes_that_cannot_stand_for_the_changes_are_ignored() {
    let mut source = appended(20);
    let bytes = source.save();
    let hashes = hashes_of(&source);
    let mut full = AutoCommit::load(&bytes).unwrap();
    let foreign = ChangeHash::try_from([7u8; 32].as_slice()).unwrap();

    let mut short = hashes.clone();
    short.pop();
    let mut duplicated = hashes.clone();
    duplicated[1] = duplicated[0];
    let mut wrong_head = hashes.clone();
    *wrong_head.last_mut().unwrap() = foreign;
    let mut wrong_root = hashes.clone();
    wrong_root[0] = foreign;
    let mut extra = hashes.clone();
    extra.push(foreign);

    for (case, hint) in [
        ("one short", short),
        ("a duplicate", duplicated),
        ("another head", wrong_head),
        ("one too many", extra),
        ("empty", Vec::new()),
    ] {
        let mut loaded = load_known(&bytes, &hint);
        assert_eq!(hashes_of(&loaded), hashes, "{case}");
        assert_same_log(&mut loaded, &mut full);
    }

    // What the patch does not check: a hint naming the right heads is
    // trusted for the rest, which is why only hashes this device listed for
    // exactly these bytes may be passed (app-state binds them by digest).
    let mut loaded = load_known(&bytes, &wrong_root);
    assert_eq!(hashes_of(&loaded).first(), Some(&foreign));
    assert_eq!(loaded.get_heads(), source.get_heads());
}
