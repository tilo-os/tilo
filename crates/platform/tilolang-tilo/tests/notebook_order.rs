//! The notebook's order: the notes stand in its tree in the order the store keeps them, and a
//! note made stands at the end of them.

use automerge::AutoCommit;
use tilolang_runtime::{Principal, UiNode, Value, ViewInstance};

use crate::fixtures::Replica;

const NOTES: &str = "so.tilo.lang.notes";

/// The notes app and the package it uses, as they ship.
fn packages() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            include_str!("../../../../apps/tl/discussion/app.tl"),
            include_str!("../../../../apps/tl/discussion/app.ids"),
        ),
        (
            include_str!("../../../../apps/tl/notes/app.tl"),
            include_str!("../../../../apps/tl/notes/app.ids"),
        ),
    ]
}

fn all<'a>(node: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    let mut out = Vec::new();
    walk(node, kind, &mut out);
    out
}

fn walk<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        walk(child, kind, out);
    }
}

/// The notes as the notebook's tree holds them: what each row is called, in the order the store
/// keeps them.
fn titled(view: &mut ViewInstance) -> Vec<String> {
    view.poll_patches().unwrap();
    all(view.tree(), "ui.TreeViewNode")
        .into_iter()
        .filter_map(|node| match node.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .collect()
}

/// The notebook shows its notes in the order the store keeps them, and a note a person makes
/// stands at the end of them.
#[test]
fn the_notebook_lists_its_notes_in_the_order_the_store_keeps_them() {
    let (mut replica, loaded) = Replica::new("ada", AutoCommit::new(), &packages());
    replica.host.root_of(NOTES).unwrap();
    replica.commit();
    let scenario = loaded
        .last()
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "a week of notes")
        .expect("Notes stages a week of notes");
    replica
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), (scenario.action.as_str(), vec![]))
        .unwrap();
    replica.commit();

    let mut notebook = replica.view(NOTES);
    notebook.set_lazy_rows(false).unwrap();
    let kept = titled(&mut notebook);
    assert_eq!(
        kept,
        [
            "Q3 plan",
            "Standups",
            "Monday",
            "Tuesday",
            "Interviews",
            "Home",
            "Groceries",
            "Lentil soup",
        ],
        "the notes in the order they were written, each under the note it stands in"
    );

    // A note made stands after the ones the notebook holds.
    let notes = Value::resource(&replica.host.root_of(NOTES).unwrap());
    let made = crate::legibility::call(&replica, "ada", NOTES, "new_note", vec![notes]);
    crate::legibility::call(
        &replica,
        "ada",
        NOTES,
        "retitle",
        vec![made, Value::text("Book list")],
    );
    replica.commit();
    let mut after = replica.view(NOTES);
    after.set_lazy_rows(false).unwrap();
    let mut expected = kept;
    expected.push("Book list".to_owned());
    assert_eq!(
        titled(&mut after),
        expected,
        "the note made stands at the end of the tree"
    );
}
