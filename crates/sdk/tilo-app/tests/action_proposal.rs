#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

//! A one-shot model answer is shown before it is written: accepting it is one
//! action on the record — one change, one undo step — and rejecting it writes
//! nothing at all.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_app::prelude::*;
use tilo_app::widget::action_proposal::{
    ActionProposal, ActionProposalView, ContextPart, DiffTag, ModelPass, ProposalChange,
    ProposalContext, ProposalDecision, ProposalTarget,
};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub title: String,
    pub body: String,
    pub pinned: bool,
}

#[resource(kind = "test.note", label = "Note", app = "so.tilo.test-note")]
impl Note {}

struct NoteApp;

impl ResourceApp for NoteApp {
    type Resource = Note;
    const ID: &'static str = "so.tilo.test-note";
    const TITLE: &'static str = "Note";

    fn view(cx: &mut ViewCx<Note>) -> Element {
        let p = cx.paths();
        Box::new(tilo_app::column![
            cx.input(p.title).with_id("title"),
            cx.input(p.body).with_id("body"),
        ])
    }
}

/// The answer a hosted model gave to a rewrite of `before`, declaring as
/// context exactly the part it was sent.
fn rewrite(before: &str, after: &str) -> ActionProposal {
    let context = ProposalContext::new(
        Some(ContextPart::new("prompt", "Rewrite this in plain words.")),
        vec![ContextPart::new("selection", before)],
    );
    ActionProposal::new(
        "proposal-rewrite",
        context,
        ProposalChange::text(before, after),
        ModelPass::hosted("Claude Sonnet"),
    )
}

#[test]
fn accepting_a_rewrite_is_one_undo_step() {
    let mut app = TestApp::<NoteApp>::new().unwrap();
    app.harness().click_named("body").unwrap();
    app.harness().type_text("the cat sat on the mat");
    assert_eq!(app.state().body, "the cat sat on the mat");

    let proposal = rewrite("the cat sat on the mat", "a cat sat on a mat");
    // Presented and accepted while the person read it: the write is one
    // action, so one Ctrl+Z puts the sentence back whole.
    let accept = proposal.accept_action(ProposalTarget::whole(Note::paths().body));
    app.invoke(accept.invoke().unwrap()).unwrap();
    assert_eq!(app.state().body, "a cat sat on a mat");

    assert!(app.undo());
    assert_eq!(
        app.state().body,
        "the cat sat on the mat",
        "one undo takes the whole answer back"
    );
}

#[test]
fn rejecting_a_rewrite_writes_nothing() {
    let mut app = TestApp::<NoteApp>::new().unwrap();
    app.harness().click_named("body").unwrap();
    app.harness().type_text("rough draft");
    let before = app.state();

    let proposal = rewrite("rough draft", "considered draft");
    assert_eq!(proposal.reject(), ProposalDecision::Reject);
    // Rejecting is the absence of a submission: nothing was invoked, so
    // nothing changed and History has nothing to show.
    assert_eq!(app.state(), before, "reject leaves the record as it was");
}

#[test]
fn a_value_answer_changes_a_field_that_is_not_text() {
    let mut app = TestApp::<NoteApp>::new().unwrap();
    assert!(!app.state().pinned);

    let context = ProposalContext::new(
        Some(ContextPart::new("prompt", "Keep this note at the top?")),
        Vec::new(),
    );
    let proposal = ActionProposal::new(
        "proposal-pin",
        context,
        ProposalChange::value("false", "true"),
        ModelPass::on_device("a small model"),
    );
    let accept = proposal.accept_action(ProposalTarget::whole(Note::paths().pinned));
    app.invoke(accept.invoke().unwrap()).unwrap();
    assert!(app.state().pinned, "the answer set the field");

    assert!(app.undo());
    assert!(!app.state().pinned, "one undo takes the answer back");
}

#[test]
fn an_answer_over_a_moved_field_is_refused() {
    let mut app = TestApp::<NoteApp>::new().unwrap();
    app.harness().click_named("body").unwrap();
    app.harness().type_text("first version");
    let proposal = rewrite("first version", "initial version");
    let accept = proposal.accept_action(ProposalTarget::selection(Note::paths().body, 0..13));

    assert!(
        accept.refusal_over(&app.state().body).is_none(),
        "the field still holds what the model was shown"
    );

    // The person types before the range the model was shown: the answer was
    // about other characters, so it is refused rather than written over them.
    app.harness().key("Home");
    app.harness().type_text("draft: ");
    assert_eq!(app.state().body, "draft: first version");
    assert!(
        accept.refusal_over(&app.state().body).is_some(),
        "the characters moved while the model ran, so it writes nothing"
    );
}

#[test]
fn the_proposal_declares_every_part_it_sends() {
    let proposal = rewrite("first line\nsecond line", "Second line, in plain words.");
    let sent = proposal.prompt();
    // What the person reads above Accept is the whole of what left the
    // machine: the prompt and the selection, in that order.
    assert!(sent.contains("Rewrite this in plain words."));
    assert!(sent.contains("first line\nsecond line"));
    assert!(proposal.refusal().is_none(), "an ordinary selection sends");

    // A selection too long to send is refused before it leaves, and says so.
    let huge = "x".repeat(64 * 1024);
    let too_long = rewrite(&huge, "shorter");
    assert!(too_long.refusal().is_some(), "an oversized part refuses");
}

#[test]
fn the_view_shows_which_model_ran_where_before_anything_is_sent() {
    let proposal = rewrite("rough", "polished");
    // Where it ran is part of what the person agrees to, so it is said
    // before the decision, not after it.
    let placed = proposal.model.placed();
    assert!(placed.contains("Claude Sonnet"), "{placed}");
    assert!(placed.contains("another machine"), "{placed}");

    // The answer as the diff the person reads: the line it replaces, then
    // the line it gives back.
    let diff = proposal.change.diff();
    assert_eq!(diff.len(), 2, "one removed line, one added line");
    assert_eq!(diff[0].tag, DiffTag::Removed);
    assert_eq!(diff[0].text, "rough");
    assert_eq!(diff[1].tag, DiffTag::Added);
    assert_eq!(diff[1].text, "polished");
}

#[test]
fn the_view_writes_nothing_until_the_person_accepts() {
    let decided: Rc<RefCell<Option<ProposalDecision>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&decided);
    let sheet = ActionProposalView::<Note>::new(
        rewrite("rough draft", "considered draft"),
        ProposalTarget::whole(Note::paths().body),
    )
    .with_decisions(move |decision| {
        *sink.borrow_mut() = Some(decision);
    });

    // Shown, not written: a proposal in front of the person submits nothing,
    // so the same Ctrl+Z is not needed for something never agreed to.
    assert!(sheet.decision().is_none());
    assert!(sheet.invoke().unwrap().is_none());
    assert!(decided.borrow().is_none());
}
