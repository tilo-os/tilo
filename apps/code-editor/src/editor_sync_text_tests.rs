use super::super::*;
use crate::doc::EditorDoc;
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};

fn fixture() -> (AutoCommit, ObjId, EditorView) {
    let mut doc = AutoCommit::new();
    let object = EditorDoc::ensure_buffer_key(&mut doc, "source", "fn original() {}\n")
        .expect("source text");
    doc.commit();
    let mut editor = EditorView::new();
    editor.bind(object.clone(), Language::Rust);
    (doc, object, editor)
}

fn observed<'doc>(doc: &'doc AutoCommit, object: &ObjId) -> SyncText<'doc> {
    SyncText {
        document: doc,
        object: object.clone(),
        text: doc.text(object).expect("observed authoritative text"),
    }
}

#[test]
fn same_sync_uses_authoritative_text_without_another_crdt_walk() {
    let (doc, object, mut editor) = fixture();
    let read = observed(&doc, &object);
    editor.sync_observed(&doc, &ROOT, Some(&read));
    assert_eq!(editor.text(), read.text);
    assert_eq!(editor.sync_text_reads(), 0);
    editor.sync(&doc, &ROOT);
    assert_eq!(editor.sync_text_reads(), 0, "stable heads still skip reads");
}

#[test]
fn different_document_with_identical_object_identity_cannot_supply_text() {
    let (doc, object, mut editor) = fixture();
    let mut changed = doc.clone();
    EditorDoc::splice(&mut changed, &object, 0, 0, "// changed\n");
    let read = observed(&doc, &object);
    editor.sync_observed(&changed, &ROOT, Some(&read));
    assert_eq!(editor.text(), changed.text(&object).unwrap());
    assert_eq!(editor.sync_text_reads(), 1);
    assert_ne!(editor.text(), read.text);
}

#[test]
fn different_object_in_the_same_document_cannot_supply_text() {
    let (mut doc, object, mut editor) = fixture();
    let other =
        EditorDoc::ensure_buffer_key(&mut doc, "other", "fn other() {}\n").expect("other source");
    let read = observed(&doc, &other);
    editor.sync_observed(&doc, &ROOT, Some(&read));
    assert_eq!(editor.text(), doc.text(&object).unwrap());
    assert_eq!(editor.sync_text_reads(), 1);
}

#[test]
fn equal_pending_counts_do_not_reuse_an_earlier_sync_observation() {
    let (doc, object, mut editor) = fixture();
    let mut first = doc.clone();
    let mut second = doc.clone();
    EditorDoc::splice(&mut first, &object, 0, 0, "// first\n");
    EditorDoc::splice(&mut second, &object, 0, 0, "// other\n");
    assert_eq!(first.pending_ops(), second.pending_ops());
    assert!(first.pending_ops() > 0);
    for current in [&first, &second, &doc] {
        let read = observed(current, &object);
        editor.sync_observed(current, &ROOT, Some(&read));
        assert_eq!(editor.text(), current.text(&object).unwrap());
    }
    assert_eq!(editor.sync_text_reads(), 0);
    assert_eq!(first.pending_ops(), second.pending_ops());
}

#[test]
fn normal_sync_retains_the_direct_read_fallback() {
    let (doc, object, mut editor) = fixture();
    editor.sync(&doc, &ROOT);
    assert_eq!(editor.text(), doc.text(&object).unwrap());
    assert_eq!(editor.sync_text_reads(), 1);
    editor.sync(&doc, &ROOT);
    assert_eq!(editor.sync_text_reads(), 1);
}
