//! A large document one reader loaded or saved is the next reader's copy, not loaded again.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT, ReadDoc as _};
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

#[test]
fn a_small_document_is_not_kept() {
    shared::clear();
    let mut doc = tracker(5);
    let bytes = doc.save();
    shared::hold(&bytes, &doc);
    assert_eq!(shared::kept(), 0);
}
