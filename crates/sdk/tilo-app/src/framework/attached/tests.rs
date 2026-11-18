#![allow(
    clippy::unwrap_used,
    reason = "in a test an unwrap is the assertion, not a hazard"
)]

use super::*;
use std::collections::BTreeSet;
use ui_core::automerge::transaction::Transactable;
use ui_core::automerge::{AutoCommit, ObjType, ROOT, ReadDoc};

/// The names on each path, without the positions in lists a whole-document
/// diff reads and a change's operations do not.
fn named(keys: Vec<Vec<String>>) -> BTreeSet<Vec<String>> {
    keys.into_iter()
        .map(|path| {
            path.into_iter()
                .filter(|key| key.parse::<usize>().is_err())
                .collect()
        })
        .collect()
}

#[test]
fn a_change_names_the_keys_it_writes_as_the_document_with_it_applied_does() {
    let mut doc = AutoCommit::new();
    let issues = doc.put_object(ROOT, "issues", ObjType::Map).unwrap();
    let first = doc.put_object(&issues, "a", ObjType::Map).unwrap();
    doc.put(&first, "title", "first").unwrap();
    doc.put_object(&first, "comments", ObjType::Map).unwrap();
    doc.put_object(ROOT, "list", ObjType::List).unwrap();
    doc.commit();
    let before = doc.get_heads();

    let mut author = doc.fork();
    let issues = author.get(ROOT, "issues").unwrap().unwrap().1;
    let made = author.put_object(&issues, "b", ObjType::Map).unwrap();
    author.put(&made, "title", "second").unwrap();
    let body = author.put_object(&made, "body", ObjType::Text).unwrap();
    author.splice_text(&body, 0, 0, "words").unwrap();
    let first = author.get(&issues, "a").unwrap().unwrap().1;
    let comments = author.get(&first, "comments").unwrap().unwrap().1;
    let comment = author.put_object(&comments, "c1", ObjType::Map).unwrap();
    author.put(&comment, "text", "a comment").unwrap();
    author.delete(&first, "title").unwrap();
    let list = author.get(ROOT, "list").unwrap().unwrap().1;
    let entry = author.insert_object(&list, 0, ObjType::Map).unwrap();
    author.put(&entry, "k", 1).unwrap();
    author.commit();
    let bytes: Vec<u8> = author
        .get_changes(&before)
        .iter()
        .flat_map(|change| change.raw_bytes().to_vec())
        .collect();

    let read = keys_written_by(&mut doc, &bytes).expect("the changes build on the document");
    let applied =
        changed_keys_of(&mut doc.fork(), &DocumentMutation::Snapshot(author.save())).unwrap();
    assert_eq!(named(read.clone()), named(applied));
    assert!(read.contains(&vec![
        "issues".into(),
        "a".into(),
        "comments".into(),
        "c1".into(),
        "text".into()
    ]));
    assert_eq!(doc.get_heads(), before, "the document itself is untouched");

    // What the document holds already writes nothing new.
    let mut holding = author.fork();
    assert_eq!(keys_written_by(&mut holding, &bytes), Some(Vec::new()));
    // Changes building on what the document has not got are read by applying them.
    let mut empty = AutoCommit::new();
    assert_eq!(keys_written_by(&mut empty, &bytes), None);
}
