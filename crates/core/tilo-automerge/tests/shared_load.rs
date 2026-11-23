//! A large document one reader loaded or saved is the next reader's copy, not loaded again.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash, ROOT, ReadDoc as _};
use tilo_automerge::shared;

/// A document large enough to keep, as a tracker of `issues` issues is.
fn tracker(issues: usize) -> AutoCommit {
    let mut doc = AutoCommit::new();
    for at in 0..issues {
        // Text that does not compress, as a tracker's issues' does not.
        let text = blake3::hash(format!("issue {at}").as_bytes())
            .to_hex()
            .to_string();
        doc.put(ROOT, format!("issue-{at}"), text).unwrap();
        if at % 50 == 0 {
            doc.commit();
        }
    }
    doc.commit();
    doc
}

#[test]
fn bytes_a_reader_holds_load_as_a_copy_of_its_document() {
    shared::clear();
    let mut doc = tracker(2_000);
    let bytes = doc.save();
    assert!(bytes.len() >= shared::MIN_BYTES);
    assert!(shared::held(&bytes).is_none(), "nobody holds it yet");

    shared::hold(&bytes, &doc);
    let mut copy = shared::held(&bytes).expect("the saver's document");
    let mut loaded = AutoCommit::load(&bytes).unwrap();
    assert_eq!(copy.get_heads(), loaded.get_heads());
    assert_eq!(copy.save(), loaded.save(), "the same document a load gives");
    assert_ne!(
        copy.get_actor(),
        doc.get_actor(),
        "under an actor of its own"
    );

    // Writing to the copy changes neither the saver's document nor the next copy.
    copy.put(ROOT, "late", "a write").unwrap();
    copy.commit();
    assert!(doc.get(ROOT, "late").unwrap().is_none());
    let again = shared::held(&bytes).unwrap();
    assert!(again.get(ROOT, "late").unwrap().is_none());

    // Other bytes, or bytes with a change appended, are not this document.
    let mut torn = bytes.clone();
    torn.extend_from_slice(&copy.save_after(&doc.get_heads())[..10]);
    assert!(shared::held(&torn).is_none());

    // A load keeps what it loaded for the next reader.
    shared::clear();
    let mut first = shared::load(&bytes).unwrap();
    assert_eq!(shared::kept(), 1);
    let mut second = shared::load(&bytes).unwrap();
    assert_eq!(first.get_heads(), second.get_heads());
    assert_ne!(first.get_actor(), second.get_actor());

    // A document with changes not committed is not what its bytes load to.
    shared::clear();
    let mut open = tracker(2_000);
    let saved = open.save();
    open.put(ROOT, "pending", "not committed").unwrap();
    shared::hold(&saved, &open);
    assert_eq!(shared::kept(), 0);
}

/// A document kept in parts is read part after part by one reader, then by the next: every part
/// the first reader loaded is still the next reader's copy, however many parts there are.
#[test]
fn every_part_one_reader_loaded_is_the_next_readers_copy() {
    shared::clear();
    let parts: Vec<Vec<u8>> = (0..24).map(|_| tracker(2_000).save()).collect();
    assert!(parts.iter().all(|bytes| bytes.len() >= shared::MIN_BYTES));
    for bytes in &parts {
        shared::load(bytes).unwrap();
    }
    assert_eq!(shared::decoded(), parts.len());
    for bytes in &parts {
        shared::load(bytes).unwrap();
    }
    assert_eq!(
        shared::decoded(),
        parts.len(),
        "the second reader loaded no part again"
    );
}

#[test]
fn a_small_document_is_not_kept() {
    shared::clear();
    let mut doc = tracker(5);
    let bytes = doc.save();
    shared::hold(&bytes, &doc);
    assert_eq!(shared::kept(), 0);
}

#[test]
fn a_document_is_found_by_its_heads_as_well_as_its_bytes() {
    shared::clear();
    let mut doc = tracker(2_000);
    // The bytes a capture hands out: the document saved whole, with what was appended since.
    let mut captured = doc.save();
    let at = doc.get_heads();
    doc.put(ROOT, "later", "a write").unwrap();
    doc.commit();
    captured.extend_from_slice(&doc.save_after(&at));
    shared::hold(&captured, &doc);

    let heads = doc.get_heads();
    let mut copy = shared::held_at(&heads).expect("the document its heads name");
    assert_eq!(copy.get_heads(), heads);
    assert_eq!(copy.save(), doc.save(), "the same document a load gives");
    assert_ne!(
        copy.get_actor(),
        doc.get_actor(),
        "under an actor of its own"
    );

    // The same document saved whole is other bytes: its heads still answer.
    let resaved = doc.save();
    assert!(shared::held(&resaved).is_none(), "nobody holds those bytes");
    assert!(shared::held_at(&heads).is_some());

    // Heads no document on this thread stands at answer none.
    assert!(shared::held_at(&[ChangeHash([0; 32])]).is_none());
    assert!(shared::held_at(&[]).is_none());

    // What is let go answers neither way.
    shared::clear();
    assert!(shared::held_at(&heads).is_none());
    assert!(shared::held(&captured).is_none());
}
