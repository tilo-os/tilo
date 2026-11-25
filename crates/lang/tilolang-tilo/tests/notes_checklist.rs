//! A note's checklist after the app wrote it and tagged the note: the tasks stay tasks, with the
//! state each was written in, whether the body came from a page's editor or from Markdown.

use std::collections::BTreeMap;

use tilo_doc::AttrVal;
use tilo_doc::snapshot::BlockView;
use tilolang_runtime::rich::RichValue;
use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef, Value};

use crate::fixtures::{Replica, target};

const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);
const APP: &str = "so.tilo.notes";

/// Ada's workspace with its notebook.
fn workspace() -> (Replica, ResourceRef) {
    let (ada, _loaded) = Replica::new(
        "ada",
        crate::fixtures::Workspace::new(),
        &[DISCUSSION, NOTES],
    );
    ada.host.set_clock(|| "2026-11-06T09:00:00Z".to_owned());
    ada.commit();
    let notebook = ada.host.root_of(APP).expect("the notebook");
    (ada, notebook)
}

/// What `action` of the notes app answered, run by `r`'s person.
fn call(r: &Replica, action: &str, args: Vec<Value>) -> Value {
    let id = r
        .host
        .package(APP)
        .expect("the notes package")
        .meta
        .actions
        .iter()
        .find(|found| found.name == action)
        .unwrap_or_else(|| panic!("the action `{action}`"))
        .id
        .clone();
    let who = Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
        r.name.clone(),
    ));
    match r.host.runtime().invoke_action(&who, (id.as_str(), args)) {
        Ok(InvokeOutcome::Done(value)) => value,
        other => panic!("`{action}` answered {other:?}"),
    }
}

/// A note made in the notebook, titled `title`.
fn note(r: &Replica, notebook: &ResourceRef, title: &str) -> ResourceRef {
    let made = target(&call(
        r,
        "new_note",
        vec![Value::resource(notebook), Value::text(title)],
    ));
    r.commit();
    made
}

/// A paragraph of a body, `attrs` set on it.
fn block(shape: &str, text: &str, attrs: &[(&str, AttrVal)]) -> BlockView {
    BlockView {
        id: String::new(),
        kind: shape.to_owned(),
        text: text.to_owned(),
        marks: Vec::new(),
        attrs: attrs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect::<BTreeMap<_, _>>(),
        children: Vec::new(),
    }
}

fn task(text: &str) -> BlockView {
    block(
        "list_item",
        text,
        &[
            ("level", AttrVal::Int(0)),
            ("ordered", AttrVal::Bool(false)),
            ("checked", AttrVal::Bool(false)),
        ],
    )
}

/// A note's body as the workspace holds it.
fn body(r: &Replica, note: &ResourceRef) -> RichValue {
    match r.host.runtime().read(note, "body") {
        Ok(Value::Rich(rich)) => rich,
        other => panic!("a note's body: {other:?}"),
    }
}

/// What each line of a body reads: how it stands, its words, and whether it is a task and done.
type Said = (String, String, Option<bool>);

/// What one step of typing writes, and how the note reads after it.
type Step = (
    Vec<BlockView>,
    Vec<(&'static str, &'static str, Option<bool>)>,
);

/// What the editor sends as the person types, step by step: the whole body each time its
/// lines changed, and how the note's body should read after it.
fn typed_states() -> Vec<Step> {
    let bullet = |text: &str| {
        block(
            "list_item",
            text,
            &[
                ("level", AttrVal::Int(0)),
                ("ordered", AttrVal::Bool(false)),
            ],
        )
    };
    let heading = || block("heading", "Packing", &[("level", AttrVal::Int(2))]);
    vec![
        (vec![heading()], vec![("heading", "Packing", None)]),
        (
            vec![heading(), block("paragraph", "", &[])],
            vec![("heading", "Packing", None), ("paragraph", "", None)],
        ),
        (
            vec![heading(), bullet("passport")],
            vec![
                ("heading", "Packing", None),
                ("list_item", "passport", None),
            ],
        ),
        (
            vec![heading(), bullet("passport"), bullet("")],
            vec![
                ("heading", "Packing", None),
                ("list_item", "passport", None),
                ("list_item", "", None),
            ],
        ),
        (
            vec![heading(), bullet("passport"), task("")],
            vec![
                ("heading", "Packing", None),
                ("list_item", "passport", None),
                ("list_item", "", Some(false)),
            ],
        ),
        (
            vec![heading(), bullet("passport"), task("charger")],
            vec![
                ("heading", "Packing", None),
                ("list_item", "passport", None),
                ("list_item", "charger", Some(false)),
            ],
        ),
        (
            vec![heading(), bullet("passport"), task("charger"), task("")],
            vec![
                ("heading", "Packing", None),
                ("list_item", "passport", None),
                ("list_item", "charger", Some(false)),
                ("list_item", "", Some(false)),
            ],
        ),
        (
            vec![
                heading(),
                bullet("passport"),
                task("charger"),
                task("adapters"),
            ],
            vec![
                ("heading", "Packing", None),
                ("list_item", "passport", None),
                ("list_item", "charger", Some(false)),
                ("list_item", "adapters", Some(false)),
            ],
        ),
    ]
}

fn written(rich: &RichValue) -> Vec<Said> {
    rich.blocks
        .iter()
        .map(|block| {
            (
                block.kind.clone(),
                block.text.clone(),
                block.bool_attr("checked"),
            )
        })
        .collect()
}

#[test]
fn a_checklist_the_editor_wrote_survives_tagging_the_note() {
    let (ada, notebook) = workspace();
    let note = note(&ada, &notebook, "Trip to Lisbon");
    // The page's editor writes the body it typed: a heading, a bullet and two tasks.
    let page = ada
        .host
        .open_resource(&note, None)
        .expect("the note's page");
    let editor = page
        .tree()
        .first_of_kind("ui.BlockEditor")
        .expect("the body's editor")
        .key
        .clone();
    page.edit(
        &editor,
        &Value::Rich(RichValue {
            styles: None,
            blocks: vec![
                block("heading", "Packing", &[("level", AttrVal::Int(2))]),
                block(
                    "list_item",
                    "passport",
                    &[
                        ("level", AttrVal::Int(0)),
                        ("ordered", AttrVal::Bool(false)),
                    ],
                ),
                task("charger"),
                task("adapters"),
            ],
        }),
    )
    .expect("the editor's write lands");
    ada.commit();
    let wrote = written(&body(&ada, &note));
    assert_eq!(
        wrote,
        [
            ("heading".to_owned(), "Packing".to_owned(), None),
            ("list_item".to_owned(), "passport".to_owned(), None),
            ("list_item".to_owned(), "charger".to_owned(), Some(false)),
            ("list_item".to_owned(), "adapters".to_owned(), Some(false)),
        ],
        "the note keeps every paragraph as the editor wrote it"
    );

    // The person tags it from its row's menu.
    call(
        &ada,
        "tag",
        vec![Value::resource(&note), Value::text("travel")],
    );
    ada.commit();
    assert_eq!(
        written(&body(&ada, &note)),
        wrote,
        "tagging the note leaves its body's tasks as they were"
    );
}

#[test]
fn each_step_of_typing_the_body_leaves_the_note_as_it_was_written() {
    let (ada, notebook) = workspace();
    let note = note(&ada, &notebook, "Trip to Lisbon");
    let page = ada
        .host
        .open_resource(&note, None)
        .expect("the note's page");
    let editor = page
        .tree()
        .first_of_kind("ui.BlockEditor")
        .expect("the body's editor")
        .key
        .clone();
    // What the editor sends as the person types: the whole body each time its lines changed.
    // What the editor sends as the person types: the whole body each time its lines changed.
    let states: Vec<Step> = typed_states();
    for (step, (blocks, want)) in states.into_iter().enumerate() {
        page.edit(
            &editor,
            &Value::Rich(RichValue {
                blocks,
                styles: None,
            }),
        )
        .unwrap_or_else(|error| panic!("step {step}: {error:?}"));
        ada.commit();
        assert_eq!(
            written(&body(&ada, &note)),
            want.iter()
                .map(|(shape, text, checked)| ((*shape).to_owned(), (*text).to_owned(), *checked))
                .collect::<Vec<_>>(),
            "step {step}: the body reads as the editor wrote it"
        );
    }
}

#[test]
fn the_workspace_reads_the_body_as_the_app_wrote_it() {
    // The app keeps its own copy; the workspace keeps the change the app hands it. Both must
    // read the body the person typed, a line each.
    let (ada, notebook) = workspace();
    let note = note(&ada, &notebook, "Trip to Lisbon");
    let workspace_copy = ada.fork("ada", &[DISCUSSION, NOTES]);
    ada.sync_with(&workspace_copy);
    let page = ada
        .host
        .open_resource(&note, None)
        .expect("the note's page");
    let editor = page
        .tree()
        .first_of_kind("ui.BlockEditor")
        .expect("the body's editor")
        .key
        .clone();
    page.edit(
        &editor,
        &Value::Rich(RichValue {
            styles: None,
            blocks: vec![
                block("heading", "Packing", &[("level", AttrVal::Int(2))]),
                block(
                    "list_item",
                    "passport",
                    &[
                        ("level", AttrVal::Int(0)),
                        ("ordered", AttrVal::Bool(false)),
                    ],
                ),
                task("charger"),
                task("adapters"),
            ],
        }),
    )
    .expect("the editor's write lands");
    // What the app hands the workspace of the write, as its process does.
    ada.commit();
    ada.send_to(&workspace_copy);
    let want = [
        ("heading".to_owned(), "Packing".to_owned(), None),
        ("list_item".to_owned(), "passport".to_owned(), None),
        ("list_item".to_owned(), "charger".to_owned(), Some(false)),
        ("list_item".to_owned(), "adapters".to_owned(), Some(false)),
    ];
    assert_eq!(
        written(&body(&ada, &note)),
        want,
        "the app reads the body it typed"
    );
    assert_eq!(
        written(&body(&workspace_copy, &note)),
        want,
        "the workspace reads the body the app recorded"
    );

    // Tagging the note goes the same way: the app's change, read by the workspace.
    call(
        &ada,
        "tag",
        vec![Value::resource(&note), Value::text("travel")],
    );
    ada.commit();
    ada.send_to(&workspace_copy);
    assert_eq!(
        written(&body(&workspace_copy, &note)),
        want,
        "tagging leaves the workspace's body as the app wrote it"
    );
}

#[test]
fn a_checklist_a_note_was_made_with_survives_tagging_it() {
    let (ada, notebook) = workspace();
    let note = target(&call(
        &ada,
        "new_note",
        vec![
            Value::resource(&notebook),
            Value::text("Trip to Lisbon"),
            Value::text("## Packing\n\n- passport\n\n- [ ] charger\n\n- [ ] adapters"),
        ],
    ));
    ada.commit();
    let wrote = written(&body(&ada, &note));
    assert_eq!(
        wrote,
        [
            ("heading".to_owned(), "Packing".to_owned(), None),
            ("list_item".to_owned(), "passport".to_owned(), None),
            ("list_item".to_owned(), "charger".to_owned(), Some(false)),
            ("list_item".to_owned(), "adapters".to_owned(), Some(false)),
        ],
        "the Markdown's checklist is the note's tasks"
    );
    call(
        &ada,
        "tag",
        vec![Value::resource(&note), Value::text("travel")],
    );
    ada.commit();
    assert_eq!(
        written(&body(&ada, &note)),
        wrote,
        "tagging the note leaves its body's tasks as they were"
    );
}
