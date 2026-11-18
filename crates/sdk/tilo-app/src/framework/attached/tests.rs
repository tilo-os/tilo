#![allow(
    clippy::unwrap_used,
    reason = "in a test an unwrap is the assertion, not a hazard"
)]

use super::*;
use std::collections::BTreeSet;
use ui_core::automerge::transaction::Transactable;
use ui_core::automerge::{AutoCommit, ObjType, ROOT, ReadDoc};

#[test]
fn a_change_names_the_keys_it_writes_from_its_operations() {
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
    // Read from the document holding them, as the replica that admitted
    // them reads them.
    let changes = author.get_changes(&before);
    let read = keys_written(&author, &changes).expect("the changes build on the document");
    let read: BTreeSet<Vec<String>> = read.into_iter().collect();
    let path = |keys: &[&str]| keys.iter().map(|key| (*key).to_owned()).collect::<Vec<_>>();
    for written in [
        path(&["issues", "b"]),
        path(&["issues", "b", "title"]),
        path(&["issues", "b", "body"]),
        path(&["issues", "a", "comments", "c1"]),
        path(&["issues", "a", "comments", "c1", "text"]),
        path(&["issues", "a", "title"]),
        path(&["list", "k"]),
    ] {
        assert!(read.contains(&written), "{written:?} in {read:?}");
    }
    // An operation on an object neither the document nor the changes hold.
    let empty = AutoCommit::new();
    assert_eq!(keys_written(&empty, &changes), None);
}
