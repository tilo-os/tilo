//! An agent changes one paragraph of a note's rich text by the address block search names it
//! with (`tl:<app>:<id>#<field>.<place>`): the paragraph is spliced, the paragraphs beside it
//! keep their ids and marks, and the change is one version of the note.

use crate::args;
use serde_json::json;
use tilo_service::Service as _;
use tilolang_runtime::store::{Store as _, VersionKind};
use tilolang_runtime::{ResourceRef, Value};
use tilolang_service::ResourceService;
use tilolang_tilo::naming::resource_id;

use crate::IntoService as _;
use crate::fixtures::call;
use crate::fixtures::{Replica, Workspace, field_id, target};

const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);
const APP: &str = "so.tilo.notes";

/// Ada's notebook, published, and a note of three paragraphs whose first and last carry a mark:
/// the note, its published id (what a caller names it by) and the id of its body field (what an
/// address names its text by).
fn notebook() -> (ResourceService, ResourceRef, String, String) {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[DISCUSSION, NOTES]);
    let meta = packages.last().unwrap().meta.clone();
    let notebook = ada.host.root_of(APP).expect("the notebook");
    ada.commit();
    let note = target(&call(
        &ada,
        "ada",
        APP,
        "new_note",
        vec![
            Value::resource(&notebook),
            Value::text("Offsite"),
            Value::text("**First** paragraph.\n\nSecond paragraph.\n\nThe **third** paragraph."),
        ],
    ));
    ada.commit();
    let mut service = ada.into_service("ada");
    service.settle();
    let body = field_id(&meta, "Note", "body");
    let label = resource_id(APP, &note.id_string());
    (service, note, label, body)
}

/// The body of `note`, as the runtime reads it now.
fn body(service: &ResourceService, note: &ResourceRef) -> tilolang_runtime::rich::RichValue {
    match service.host().runtime().read(note, "body").unwrap() {
        Value::Rich(rich) => rich,
        other => panic!("a note's body is rich text: {other:?}"),
    }
}

/// The words of the paragraphs of `note`, in order.
fn paragraphs(service: &ResourceService, note: &ResourceRef) -> Vec<String> {
    body(service, note)
        .blocks
        .iter()
        .map(|block| block.text.clone())
        .collect()
}

/// The agent that edits the notes: a task acting for Ada.
fn agent() -> tilo_actions::ActionInvoker {
    tilo_actions::ActionInvoker::agent("task-1", "ada").acting_for("ada")
}

#[test]
fn an_agent_sets_one_paragraph_by_the_address_block_search_gives_it() {
    let (mut service, note, label, body_field) = notebook();
    let before = body(&service, &note);
    let marked = |at: usize| {
        before.blocks[at]
            .marks
            .iter()
            .any(|mark| mark.name == "strong")
    };
    assert!(marked(0) && marked(2), "the note's own marks: {before:?}");
    let versions = service.host().store().versions(&note).unwrap().len();

    // The second paragraph is set, by the address block search gives: the note, its body field
    // (by the id the document stores it under) and the paragraph's place.
    let started = service
        .invoke_as(
            &agent(),
            &label,
            "edit-block",
            &args(json!({
                "block": format!("{label}#{body_field}.1"),
                "text": "The budget is for bread, with **bold**.",
            })),
        )
        .expect("the paragraph is set");
    assert!(
        matches!(started, tilo_service::Started::Done(_)),
        "{started:?}"
    );

    let after = body(&service, &note);
    assert_eq!(
        paragraphs(&service, &note),
        [
            "First paragraph.",
            "The budget is for bread, with bold.",
            "The third paragraph.",
        ],
        "one paragraph changed, the others as they were"
    );
    assert_eq!(
        after.blocks[0], before.blocks[0],
        "the paragraph before keeps its id, its marks and its text"
    );
    assert_eq!(
        after.blocks[2], before.blocks[2],
        "the paragraph after keeps its id, its marks and its text"
    );
    assert!(
        after.blocks[1]
            .marks
            .iter()
            .any(|mark| mark.name == "strong"),
        "the paragraph set reads with the mark its Markdown gives it: {after:?}"
    );

    // History keeps the change as one version of the note, of the action it was made by.
    let versions_after = service.host().store().versions(&note).unwrap();
    assert_eq!(
        versions_after.len(),
        versions + 1,
        "one change, one version: {versions_after:?}"
    );
    let last = versions_after.last().unwrap();
    assert_eq!(last.action, "edit-block");
    assert_eq!(last.kind, VersionKind::Changed);
}

#[test]
fn an_agent_puts_a_paragraph_in_and_removes_one_by_their_addresses() {
    let (mut service, note, label, body_field) = notebook();
    // A paragraph is put in by its place, named with the field's own name and no resource: the
    // address is the action's resource when it names none.
    service
        .invoke_as(
            &agent(),
            &label,
            "insert-block",
            &args(json!({ "block": "body.1", "text": "A line put before it." })),
        )
        .expect("a paragraph is put in");
    assert_eq!(
        paragraphs(&service, &note),
        [
            "First paragraph.",
            "A line put before it.",
            "Second paragraph.",
            "The third paragraph.",
        ]
    );

    // And one is taken out.
    service
        .invoke_as(
            &agent(),
            &label,
            "delete-block",
            &args(json!({ "block": format!("{label}#{body_field}.0") })),
        )
        .expect("the paragraph is removed");
    assert_eq!(
        paragraphs(&service, &note),
        [
            "A line put before it.",
            "Second paragraph.",
            "The third paragraph.",
        ]
    );

    // A place the text does not hold is refused with what it holds.
    let refused = service
        .invoke_as(
            &agent(),
            &label,
            "delete-block",
            &args(json!({ "block": format!("{label}#{body_field}.9") })),
        )
        .expect_err("there is no paragraph 9");
    assert!(
        format!("{refused:?}").contains("paragraph 9"),
        "the refusal says which place the text does not hold: {refused:?}"
    );
}
