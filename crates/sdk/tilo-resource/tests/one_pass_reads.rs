//! A stored value read in one walk of each map is exactly what
//! reading it key by key reads: the winning value of a conflict, a counter
//! with its increments, the id of the op that wrote each key.

#![allow(
    clippy::unwrap_used,
    reason = "fixture builders shared by the tests: a failing write is a failing test"
)]

use std::collections::BTreeMap;

use automerge::transaction::Transactable;
use automerge::{ActorId, ObjType, ROOT, ReadDoc, ScalarValue};
use tilo_automerge::ApplyIncoming;
use tilo_resource::{Doc, Fields, Model, read_key, read_slots, write_key};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Card {
    pub title: String,
    pub done: bool,
    #[model(rename = "n")]
    pub count: u32,
    pub tags: BTreeMap<String, i64>,
    pub note: Option<String>,
    #[model(skip)]
    pub cache: u8,
}

/// `Card` read field by field, as `#[derive(Model)]` read it before.
fn card_key_by_key(doc: &Doc, obj: &automerge::ObjId) -> Card {
    Card {
        title: read_key(doc, obj, "title").unwrap(),
        done: read_key(doc, obj, "done").unwrap(),
        count: read_key(doc, obj, "n").unwrap(),
        tags: read_key(doc, obj, "tags").unwrap(),
        note: read_key(doc, obj, "note").unwrap(),
        cache: 0,
    }
}

/// A card with history under every key, then concurrent edits from two
/// actors merged: conflicting scalars, a conflicting map entry, a counter
/// incremented on both sides, a deleted key.
fn contested() -> Doc {
    let mut doc = Doc::new().with_actor(ActorId::from(b"left".as_slice()));
    let mut card = Card {
        title: "first".into(),
        count: 1,
        ..Card::default()
    };
    write_key(&mut doc, &ROOT, "card", &card).unwrap();
    doc.commit();
    for round in 0..5_i64 {
        card.title = format!("title {round}");
        card.count = u32::try_from(round).unwrap();
        card.done = round % 2 == 0;
        card.tags.insert(format!("t{round}"), round);
        write_key(&mut doc, &ROOT, "card", &card).unwrap();
        doc.commit();
    }
    doc.put(&ROOT, "hits", ScalarValue::counter(1)).unwrap();
    doc.put(&ROOT, "gone", "soon").unwrap();
    doc.commit();
    let mut right = doc.fork().with_actor(ActorId::from(b"right".as_slice()));
    let mut left_card = card.clone();
    left_card.done = true;
    left_card.count = 7;
    left_card.tags.insert("both".into(), 1);
    left_card.note = Some("left".into());
    write_key(&mut doc, &ROOT, "card", &left_card).unwrap();
    doc.increment(&ROOT, "hits", 2).unwrap();
    doc.commit();
    let mut right_card = card;
    right_card.done = false;
    right_card.count = 9;
    right_card.tags.insert("both".into(), 2);
    right_card.tags.remove("t0");
    right_card.note = Some("right".into());
    write_key(&mut right, &ROOT, "card", &right_card).unwrap();
    right.increment(&ROOT, "hits", 3).unwrap();
    right.delete(&ROOT, "gone").unwrap();
    right.put_object(&ROOT, "both", ObjType::Map).unwrap();
    right.commit();
    doc.put(&ROOT, "both", "scalar").unwrap();
    doc.commit();
    doc.merge_incoming(&mut right).unwrap();
    doc
}

#[test]
fn slots_are_what_get_answers_per_key() {
    let doc = contested();
    let keys = ["both", "card", "gone", "hits", "absent"];
    let slots = read_slots(&doc, &ROOT, keys);
    for (key, slot) in keys.iter().zip(slots) {
        let got = doc.get(&ROOT, *key).unwrap();
        let got = got.map(|(value, id)| (value.into_owned(), id));
        assert_eq!(slot, got, "slot {key}");
    }
    let Some((value, _)) = doc.get(&ROOT, "hits").unwrap() else {
        panic!("the counter is stored");
    };
    assert_eq!(value.as_i64(), Some(6), "both increments are counted");
    assert!(
        doc.get_all(&ROOT, "both").unwrap().len() == 2,
        "the fixture holds a conflict"
    );
}

#[test]
fn a_derived_struct_reads_what_key_by_key_reads() {
    let doc = contested();
    let Some((value, card)) = doc.get(&ROOT, "card").unwrap() else {
        panic!("the card is stored");
    };
    let walked = Card::load(&doc, &value, &card).unwrap();
    assert_eq!(walked, card_key_by_key(&doc, &card));
    assert_eq!(walked, Card::read_fields(&doc, &card).unwrap());
    assert!(walked.tags.contains_key("both"));
    assert!(walked.note.is_some());
}

#[test]
fn a_derived_struct_reports_the_first_bad_field_in_declaration_order() {
    let mut doc = Doc::new();
    let card = doc.put_object(&ROOT, "card", ObjType::Map).unwrap();
    // Both `done` and `n` hold the wrong shape; `done` is declared first.
    doc.put(&card, "n", "not a number").unwrap();
    doc.put(&card, "done", "not a bool").unwrap();
    let error = Card::read_fields(&doc, &card).unwrap_err();
    assert!(error.to_string().starts_with("done:"), "{error}");
}
