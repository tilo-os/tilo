//! What a saved document was, kept beside it: its change hashes, for a load that need not rebuild
//! them, and the heads it stood at, for a reader that already holds it.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ROOT};
use tilo_automerge::load_hint::{
    MIN_HINTED_BYTES, MIN_HINTED_CHANGES, encode, hashes_for, heads_for,
};

/// A document worth a hint, as a tracker of many issues is.
fn large() -> AutoCommit {
    let mut doc = AutoCommit::new();
    let items = doc.put_object(ROOT, "items", ObjType::Map).unwrap();
    for i in 0..4_000_u32 {
        doc.put(
            &items,
            format!("item-{i:05}"),
            blake3::hash(&i.to_le_bytes()).to_hex().to_string(),
        )
        .unwrap();
        if i % 100 == 0 {
            doc.commit();
        }
    }
    doc.commit();
    doc
}

#[test]
fn a_hint_names_exactly_the_document_chunk_it_was_made_for() {
    let mut doc = large();
    let saved = doc.save();
    let hashes = doc.committed_document().change_hashes().to_vec();
    let heads = doc.committed_heads();
    let hint = encode(&saved, &hashes, &heads).expect("a large document is hinted");
    assert_eq!(hashes_for(&hint, &saved), Some(hashes.clone()));
    assert_eq!(heads_for(&hint, &saved), Some(heads));

    // Changes appended after the chunk keep its hashes and change the heads.
    let mut grown = saved.clone();
    let sent = doc.get_heads();
    doc.put(ROOT, "later", 1).unwrap();
    doc.commit();
    grown.extend(doc.save_after(&sent));
    assert_eq!(hashes_for(&hint, &grown), Some(hashes));
    assert_eq!(heads_for(&hint, &grown), None, "an append is other bytes");
    assert_eq!(heads_for(&hint, &saved[..saved.len() - 1]), None);

    // A replacement makes both stale.
    let replaced = doc.save();
    assert_eq!(hashes_for(&hint, &replaced), None);
    assert_eq!(heads_for(&hint, &replaced), None);
    let mut flipped = saved.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    assert_eq!(hashes_for(&hint, &flipped), None);
    assert_eq!(heads_for(&hint, &flipped), None);
    assert_eq!(hashes_for(&hint, &saved[..saved.len() / 2]), None);
    assert_eq!(heads_for(&hint, &saved[..saved.len() / 2]), None);
    assert_eq!(hashes_for(&hint, &grown[saved.len()..]), None);
    assert_eq!(heads_for(&hint, &grown[saved.len()..]), None);

    assert_eq!(hashes_for(&hint[..hint.len() - 1], &saved), None);
    assert_eq!(hashes_for(&hint[1..], &saved), None);
    assert_eq!(heads_for(&hint[..hint.len() - 1], &saved), None);
    assert_eq!(heads_for(&hint[1..], &saved), None);
    assert_eq!(hashes_for(b"", &saved), None);
    assert_eq!(heads_for(b"", &saved), None);
}

#[test]
fn a_hint_without_hashes_still_names_the_heads() {
    // What a page's replication keeps when it stores the document its workspace sent: it has the
    // heads, not the whole list of changes.
    let mut doc = large();
    let saved = doc.save();
    let heads = doc.committed_heads();
    let hint = encode(&saved, &[], &heads).expect("a large document is hinted");
    assert_eq!(hashes_for(&hint, &saved), None);
    assert_eq!(heads_for(&hint, &saved), Some(heads));
}

#[test]
fn a_small_document_gets_no_hint() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "small", 1).unwrap();
    let saved = doc.save();
    let hashes = doc.committed_document().change_hashes().to_vec();
    let heads = doc.committed_heads();
    assert!(saved.len() < MIN_HINTED_BYTES);
    assert_eq!(encode(&saved, &hashes, &heads), None);
}

#[test]
fn a_small_document_of_many_changes_gets_a_hint() {
    // The desktop document's shape: one small change per workspace switch.
    let mut doc = AutoCommit::new();
    for i in 0..MIN_HINTED_CHANGES {
        doc.put(ROOT, "active", u64::try_from(i % 6).unwrap())
            .unwrap();
        doc.commit();
    }
    let saved = doc.save();
    let hashes = doc.committed_document().change_hashes().to_vec();
    let heads = doc.committed_heads();
    assert!(saved.len() < MIN_HINTED_BYTES, "{} bytes", saved.len());
    let hint = encode(&saved, &hashes, &heads).expect("many changes are hinted");
    assert_eq!(hashes_for(&hint, &saved), Some(hashes));
    assert_eq!(heads_for(&hint, &saved), Some(heads));
}
