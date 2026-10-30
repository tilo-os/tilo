//! Undo takes back the undoer's own change and leaves everyone else's alone:
//! never refused because somebody else changed the same record since.

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{ActorId, AutoCommit, ObjId, ObjType, ROOT, ReadDoc};
use tilo_actions::ActionContext;

use super::*;
use crate::history_source::TestWords;

const GENESIS: &[u8] = b"genesis";
const ALICE: &[u8] = b"alice";
const BOB: &[u8] = b"bob";
const AGENT: &[u8] = b"agent";
const RUN: &str = "run:tidy";

fn scope(actor: &[u8]) -> Scope {
    Scope::Actor(ActorId::from(actor).to_hex_string())
}

#[allow(clippy::unnecessary_wraps, reason = "the shape every compensator has")]
fn plain(_: &Version) -> Result<Option<Inverse>, String> {
    Ok(None)
}

/// `actor` makes one change with `edit`, in `run` when it names one.
fn step(doc: &mut AutoCommit, actor: &[u8], run: Option<&str>, edit: impl FnOnce(&mut AutoCommit)) {
    doc.set_actor(ActorId::from(actor));
    edit(doc);
    match run {
        Some(run) => {
            let message = ActionContext::default()
                .with_group(run, "Tidy", AGENT_RUN)
                .change_message()
                .unwrap();
            doc.commit_with(CommitOptions::default().with_message(message));
        }
        None => {
            doc.commit();
        }
    }
}

/// `actor` undoes the last of their steps (of `scope`), as a change of their own.
fn undo_as(doc: &mut AutoCommit, actor: &[u8], scope: &Scope) -> Vec<Version> {
    doc.set_actor(ActorId::from(actor));
    let undone = super::undo(doc, scope, 1, plain, &TestWords).expect("undo is never refused");
    doc.commit();
    undone
}

/// A record whose note body and title nobody's step created.
fn note(body: &str) -> (AutoCommit, ObjId, ObjId) {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(GENESIS));
    let text = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
    doc.splice_text(&text, 0, 0, body).unwrap();
    let title = doc.put_object(ROOT, "title", ObjType::Text).unwrap();
    doc.put(ROOT, "done", false).unwrap();
    doc.commit();
    (doc, text, title)
}

fn insert(doc: &mut AutoCommit, text: &ObjId, after: &str, inserted: &str) {
    let at = doc.text(text).unwrap().find(after).unwrap() + after.len();
    doc.splice_text(text, at, 0, inserted).unwrap();
}

fn delete(doc: &mut AutoCommit, text: &ObjId, gone: &str) {
    let at = doc.text(text).unwrap().find(gone).unwrap();
    doc.splice_text(text, at, isize::try_from(gone.len()).unwrap(), "")
        .unwrap();
}

#[test]
fn two_people_typing_in_different_places_each_undo_only_their_own_text() {
    let (mut doc, body, _) = note("Shopping.\nChores.");
    step(&mut doc, ALICE, None, |doc| {
        insert(doc, &body, "Shopping.", " Milk and eggs.")
    });
    step(&mut doc, BOB, None, |doc| {
        insert(doc, &body, "Chores.", " Bins out.")
    });
    assert_eq!(
        doc.text(&body).unwrap(),
        "Shopping. Milk and eggs.\nChores. Bins out."
    );

    assert_eq!(undo_as(&mut doc, ALICE, &scope(ALICE)).len(), 1);
    assert_eq!(doc.text(&body).unwrap(), "Shopping.\nChores. Bins out.");
    assert_eq!(undo_as(&mut doc, BOB, &scope(BOB)).len(), 1);
    assert_eq!(doc.text(&body).unwrap(), "Shopping.\nChores.");
}

#[test]
fn edits_in_one_paragraph_are_each_undone_by_their_author_alone() {
    let (mut doc, body, _) = note("The fox jumps over the lazy dog. It was sunny.");
    step(&mut doc, ALICE, None, |doc| {
        insert(doc, &body, "The", " quick")
    });
    step(&mut doc, ALICE, None, |doc| delete(doc, &body, " lazy"));
    step(&mut doc, BOB, None, |doc| {
        insert(doc, &body, "fox", " (red)")
    });
    step(&mut doc, BOB, None, |doc| {
        insert(doc, &body, "It was", " still")
    });
    assert_eq!(
        doc.text(&body).unwrap(),
        "The quick fox (red) jumps over the dog. It was still sunny."
    );

    // Alice's newest step comes back first: the word she deleted, back
    // where it was, with Bob's words around it.
    undo_as(&mut doc, ALICE, &scope(ALICE));
    assert_eq!(
        doc.text(&body).unwrap(),
        "The quick fox (red) jumps over the lazy dog. It was still sunny."
    );
    undo_as(&mut doc, ALICE, &scope(ALICE));
    assert_eq!(
        doc.text(&body).unwrap(),
        "The fox (red) jumps over the lazy dog. It was still sunny."
    );
    undo_as(&mut doc, BOB, &scope(BOB));
    undo_as(&mut doc, BOB, &scope(BOB));
    assert_eq!(
        doc.text(&body).unwrap(),
        "The fox jumps over the lazy dog. It was sunny."
    );
}

#[test]
fn words_typed_at_the_same_place_at_the_same_time_are_each_undone_by_their_author() {
    let (mut doc, body, _) = note("The fox.");
    let mut other = doc.fork();
    step(&mut doc, ALICE, None, |doc| {
        insert(doc, &body, "The", " quick")
    });
    step(&mut other, BOB, None, |doc| {
        insert(doc, &body, "The", " brown")
    });
    doc.merge(&mut other).unwrap();
    let both = doc.text(&body).unwrap();
    assert!(both.contains(" quick") && both.contains(" brown"), "{both}");

    undo_as(&mut doc, ALICE, &scope(ALICE));
    assert_eq!(doc.text(&body).unwrap(), "The brown fox.");
    undo_as(&mut doc, BOB, &scope(BOB));
    assert_eq!(doc.text(&body).unwrap(), "The fox.");
}

#[test]
fn a_line_somebody_added_under_yours_stays_and_text_they_typed_onto_yours_keeps_it() {
    let (mut doc, body, title) = note("Notes:");
    step(&mut doc, ALICE, None, |doc| {
        insert(doc, &body, "Notes:", "\nCall the bank.");
        doc.splice_text(&title, 0, 0, "Trip").unwrap();
    });
    step(&mut doc, BOB, None, |doc| {
        insert(doc, &body, "bank.", "\nBook the train.");
        doc.splice_text(&title, 4, 0, " to Rome").unwrap();
    });
    undo_as(&mut doc, ALICE, &scope(ALICE));
    assert_eq!(doc.text(&body).unwrap(), "Notes:\nBook the train.");
    assert_eq!(
        doc.text(&title).unwrap(),
        "Trip to Rome",
        "Bob set the title after Alice: his stays whole"
    );
}

#[test]
fn a_title_somebody_else_set_since_keeps_their_value_and_the_step_counts_as_undone() {
    let (mut doc, _, title) = note("");
    step(&mut doc, ALICE, None, |doc| {
        doc.splice_text(&title, 0, 0, "Milk").unwrap();
        doc.put(ROOT, "done", true).unwrap();
    });
    // Bob renames it the way a field stores a new value: one splice of the
    // span that differs.
    step(&mut doc, BOB, None, |doc| {
        doc.splice_text(&title, 0, 1, "Oat m").unwrap();
        doc.put(ROOT, "done", false).unwrap();
    });
    assert_eq!(doc.text(&title).unwrap(), "Oat milk");

    let undone = undo_as(&mut doc, ALICE, &scope(ALICE));
    assert_eq!(undone.len(), 1);
    assert_eq!(doc.text(&title).unwrap(), "Oat milk", "Bob's title stays");
    assert_eq!(
        doc.get(ROOT, "done").unwrap().unwrap().0.as_bool(),
        Some(false),
        "Bob's checkbox stays"
    );
    assert!(
        undo_preview(&doc, &scope(ALICE), 1).is_empty(),
        "Alice's step counts as undone"
    );
}

#[test]
fn a_title_somebody_cleared_and_retyped_keeps_their_value() {
    let (mut doc, _, title) = note("");
    step(&mut doc, ALICE, None, |doc| {
        doc.splice_text(&title, 0, 0, "Old").map(drop).unwrap()
    });
    step(&mut doc, ALICE, None, |doc| {
        doc.splice_text(&title, 0, 3, "").map(drop).unwrap()
    });
    step(&mut doc, BOB, None, |doc| {
        doc.splice_text(&title, 0, 0, "New").map(drop).unwrap()
    });
    undo_as(&mut doc, ALICE, &scope(ALICE));
    assert_eq!(doc.text(&title).unwrap(), "New");
}

#[test]
fn a_value_nobody_changed_since_is_restored() {
    let (mut doc, _, title) = note("");
    step(&mut doc, ALICE, None, |doc| {
        doc.splice_text(&title, 0, 0, "Milk").unwrap();
        doc.put(ROOT, "done", true).unwrap();
    });
    step(&mut doc, BOB, None, |doc| {
        doc.put(ROOT, "due", "2026-11-02").map(drop).unwrap()
    });
    undo_as(&mut doc, ALICE, &scope(ALICE));
    assert_eq!(doc.text(&title).unwrap(), "");
    assert_eq!(
        doc.get(ROOT, "done").unwrap().unwrap().0.as_bool(),
        Some(false)
    );
    assert!(doc.get(ROOT, "due").unwrap().is_some(), "Bob's date stays");
}

/// A list of items, each a map with a title, nobody's step created.
fn items(titles: &[&str]) -> (AutoCommit, ObjId) {
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(GENESIS));
    let list = doc.put_object(ROOT, "items", ObjType::List).unwrap();
    for (at, title) in titles.iter().enumerate() {
        let item = doc.insert_object(&list, at, ObjType::Map).unwrap();
        let text = doc.put_object(&item, "title", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, title).unwrap();
    }
    doc.commit();
    (doc, list)
}

fn titles(doc: &AutoCommit, list: &ObjId) -> Vec<String> {
    doc.values(list)
        .map(|(_, item)| {
            let (_, text) = doc.get(&item, "title").unwrap().unwrap();
            doc.text(&text).unwrap()
        })
        .collect()
}

fn title_of(doc: &AutoCommit, list: &ObjId, at: usize) -> ObjId {
    let (_, item) = doc.get(list, at).unwrap().unwrap();
    doc.get(&item, "title").unwrap().unwrap().1
}

#[test]
fn a_removed_item_comes_back_after_its_neighbours_changed() {
    let (mut doc, list) = items(&["Eggs", "Milk", "Bread"]);
    step(&mut doc, ALICE, None, |doc| doc.delete(&list, 1).unwrap());
    step(&mut doc, BOB, None, |doc| {
        let eggs = title_of(doc, &list, 0);
        doc.splice_text(&eggs, 4, 0, " (free range)").unwrap();
        let bread = title_of(doc, &list, 1);
        doc.splice_text(&bread, 0, 5, "Rye").unwrap();
        let butter = doc.insert_object(&list, 1, ObjType::Map).unwrap();
        let text = doc.put_object(&butter, "title", ObjType::Text).unwrap();
        doc.splice_text(&text, 0, 0, "Butter").unwrap();
    });
    undo_as(&mut doc, ALICE, &scope(ALICE));
    assert_eq!(
        titles(&doc, &list),
        ["Eggs (free range)", "Milk", "Butter", "Rye"],
        "Milk is back after Eggs, where it was"
    );
}

#[test]
fn an_agent_run_undone_while_a_person_edited_the_same_record_keeps_the_persons_edits() {
    let (mut doc, body, title) = note("Plan the trip.");
    step(&mut doc, AGENT, Some(RUN), |doc| {
        doc.splice_text(&title, 0, 0, "Trip").unwrap();
        insert(doc, &body, "trip.", " Book the train.");
        doc.put(ROOT, "done", true).unwrap();
    });
    step(&mut doc, AGENT, Some(RUN), |doc| {
        insert(doc, &body, "train.", " Pack.")
    });
    // Alice edits the same record: the title, the body, the checkbox.
    step(&mut doc, ALICE, None, |doc| {
        doc.splice_text(&title, 4, 0, "s").unwrap();
        insert(doc, &body, "Plan", " and enjoy");
        doc.put(ROOT, "done", false).unwrap();
    });

    doc.set_actor(ActorId::from(ALICE));
    let undone = super::undo(
        &mut doc,
        &Scope::Run(RUN.into()),
        usize::MAX,
        plain,
        &TestWords,
    )
    .expect("an agent run's undo is never refused");
    doc.commit();
    assert_eq!(undone.len(), 2, "both steps of the run");
    assert_eq!(doc.text(&body).unwrap(), "Plan and enjoy the trip.");
    assert_eq!(
        doc.text(&title).unwrap(),
        "Trips",
        "Alice edited the run's title"
    );
    assert_eq!(
        doc.get(ROOT, "done").unwrap().unwrap().0.as_bool(),
        Some(false)
    );
}
