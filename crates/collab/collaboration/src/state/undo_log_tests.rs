#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT};
use collab_types::MemberId;

use super::{READ, UndoLog};

const WINDOW: super::InstanceId = 1;

/// Write and close one step of `author`'s.
fn step(doc: &mut AutoCommit, history: &mut UndoLog, author: &MemberId, word: usize) {
    let before = doc.get_heads();
    doc.put(ROOT, "word", word.to_string()).unwrap();
    doc.commit();
    let after = doc.get_heads();
    history.record_committed_span(
        doc,
        before,
        after,
        None,
        None,
        Some(author.clone()),
        Some(WINDOW),
    );
    assert!(history.next_undo_in(doc, author, WINDOW).is_some());
}

/// How many changes asking for the next undo reads after one more step,
/// once `steps` steps were taken and asked about.
fn read_for_next_step(steps: usize) -> usize {
    let author = MemberId::person("ada");
    let mut doc = AutoCommit::new();
    let mut history = UndoLog::new(&mut doc);
    for word in 0..steps {
        step(&mut doc, &mut history, &author, word);
    }
    READ.with(|read| read.set(0));
    step(&mut doc, &mut history, &author, steps);
    READ.with(std::cell::Cell::get)
}

#[test]
fn asking_for_the_next_undo_reads_what_came_since_not_the_session() {
    let short = read_for_next_step(20);
    let long = read_for_next_step(400);
    assert!(short > 0, "the step was read");
    assert_eq!(
        short, long,
        "the next undo reads the same however many steps came before"
    );
}

#[test]
fn a_change_no_step_closed_over_stays_out_of_every_step() {
    let author = MemberId::person("ada");
    let mut doc = AutoCommit::new();
    let mut history = UndoLog::new(&mut doc);
    step(&mut doc, &mut history, &author, 0);
    let closed = doc.get_heads();
    // Written and asked about before any step closes over it.
    doc.put(ROOT, "word", "open").unwrap();
    doc.commit();
    let next = history.next_undo_in(&doc, &author, WINDOW).unwrap();
    assert_eq!(next.after, closed, "the open change is nobody's step yet");
    // Closing a step over it makes it the next undo.
    let open = doc.get_heads();
    history.record_committed_span(
        &doc,
        closed,
        open.clone(),
        None,
        None,
        Some(author.clone()),
        Some(WINDOW),
    );
    let next = history.next_undo_in(&doc, &author, WINDOW).unwrap();
    assert_eq!(next.after, open);
}
