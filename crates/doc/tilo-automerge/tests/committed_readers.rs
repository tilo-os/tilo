//! Reading a document's committed history through a shared reference.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc};

#[test]
fn committed_readers_see_what_get_heads_and_diff_see_once_committed() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "count", 1_i64).unwrap();
    let first = doc.get_heads();
    doc.put(ROOT, "count", 2_i64).unwrap();
    let second = doc.get_heads();

    let shared = &doc;
    assert_eq!(shared.committed_heads(), second);
    assert_eq!(shared.committed_document().get_heads(), second);
    assert_eq!(
        shared.committed_diff(&first, &second),
        doc.clone().diff(&first, &second)
    );
    let past = shared.committed_fork_at(&first).unwrap();
    assert_eq!(
        past.get(ROOT, "count").unwrap().unwrap().0.as_i64(),
        Some(1)
    );

    // A transaction still open is not part of the committed document.
    doc.put(ROOT, "count", 3_i64).unwrap();
    assert_eq!(doc.committed_heads(), second);
    let third = doc.get_heads();
    assert_ne!(third, second);
    assert_eq!(doc.committed_heads(), third);
}
