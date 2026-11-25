//! What an undo costs in a record far larger than the step it takes back.

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc};
use tilo_resource_store::history::authors::{HistoryAuthor, ResolveAuthor, UNKNOWN_AUTHOR_NAME};
use tilo_resource_store::history::verbs::HistoryEdit;
use tilo_resource_store::history::words::HistoryWords;

#[global_allocator]
static HEAP: tilo_automerge::heap::ThreadCounting = tilo_automerge::heap::ThreadCounting;

/// The host's words: a person by their actor, a step by its purpose.
struct Words;

impl ResolveAuthor for Words {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        HistoryAuthor::new(UNKNOWN_AUTHOR_NAME, 0.0, actor_hex)
    }
}

impl HistoryWords for Words {
    fn sentence(&self, entry: &tilo_actions::ActionContext) -> String {
        entry.purpose.clone().unwrap_or_default()
    }
}

fn title(doc: &AutoCommit) -> Option<String> {
    let (value, _) = doc.get(ROOT, "title").ok()??;
    value.into_string().ok()
}

/// Undoing a one-word step in a record that holds a mebibyte of text costs that word: the undo
/// reads and writes what the step changed, and copies nothing of the rest of the record.
#[test]
fn undo_of_a_small_step_costs_the_step_not_the_record() {
    const MEBIBYTE: usize = 1 << 20;
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(b"grace".as_slice()));
    let body = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
    doc.splice_text(&body, 0, 0, &"pasted ".repeat(MEBIBYTE / 7 + 1))
        .unwrap();
    doc.commit();
    doc.set_actor(ActorId::from(b"ada".as_slice()));
    doc.put(ROOT, "title", "Draft").unwrap();
    doc.commit();

    let undo = HistoryEdit::Undo {
        steps: None,
        invocation: None,
        by: None,
        to: None,
        step: None,
        chosen: None,
    };
    // Reading the text once: what an undo that walked the record would cost at least.
    let reading = tilo_clock::now();
    assert!(doc.text(&body).unwrap().len() > MEBIBYTE);
    let read = reading.elapsed();
    let undoing = tilo_clock::now();
    let cost = tilo_automerge::heap::allocated_by(|| {
        undo.apply_registered(&mut doc, |_| {}, &Words)
            .expect("undo");
        doc.commit();
    });
    let took = undoing.elapsed();

    assert_eq!(title(&doc), None, "the undo took the title back");
    assert_eq!(doc.length(&body), "pasted ".len() * (MEBIBYTE / 7 + 1));
    assert!(
        cost < MEBIBYTE / 8,
        "the undo allocated {cost} bytes for a one-word step"
    );
    assert!(
        took < read,
        "the undo took {took:?}, reading the text {read:?}"
    );
}
