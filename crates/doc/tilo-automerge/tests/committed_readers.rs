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

    doc.put(ROOT, "count", 3_i64).unwrap();
    assert_eq!(doc.committed_heads(), second);
    let third = doc.get_heads();
    assert_ne!(third, second);
    assert_eq!(doc.committed_heads(), third);
}

/// A writer that saves what is committed while someone else still holds a
/// transaction open stores only the committed changes: the open
/// transaction's ops are neither saved nor committed by the save.
#[test]
fn saving_the_committed_document_leaves_an_open_transaction_out() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "kept", 1_i64).unwrap();
    let committed = doc.get_heads();
    doc.put(ROOT, "pending", 2_i64).unwrap();
    doc.put(ROOT, "kept", 3_i64).unwrap();

    let full = doc.save_committed();
    let after = doc.committed_document().save_after(&[]);
    assert_eq!(doc.committed_heads(), committed, "nothing was committed");
    for bytes in [full, after] {
        let loaded = AutoCommit::load(&bytes).unwrap();
        assert_eq!(loaded.committed_heads(), committed);
        assert_eq!(loaded.get(ROOT, "pending").unwrap(), None);
        assert_eq!(
            loaded.get(ROOT, "kept").unwrap().unwrap().0.as_i64(),
            Some(1)
        );
    }
    assert_eq!(doc.rollback(), 2, "the transaction is still open");
}
