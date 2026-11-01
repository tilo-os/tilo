//! A room's tilolang records as session resources: a note made through the notes app's own
//! action is published, its reading is what a runtime loading notes as another app's package
//! reads back, and a change to the note moves the reading.

use automerge::AutoCommit;
use serde_json::{Map, Value as Json, json};
use tilo_service::{Service, Started};
use tilolang::IdsLock;
use tilolang_runtime::{Principal, ResourceRef, UiNode, Value};
use tilolang_tilo::host::Host;
use tilolang_tilo::publish::resource_id;
use tilolang_tilo::records_service::{RecordService, reading_type};

use crate::fixtures::{Replica, field_id};

const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);
const APP: &str = "so.tilo.lang.notes";

/// A view of another app's package that shows a note by the notes app's own row view.
const CARD: &str = r#"app test.card "Card"

use so.tilo.lang.notes as notes

view note_card(n: notes.Note) {
  notes.note_row(n)
}
"#;

/// Ada's room with its notebook, published.
fn service() -> (RecordService, tilolang_runtime::Meta) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[NOTES]);
    ada.host.root_of(APP).unwrap();
    ada.commit();
    let Replica { doc, host, .. } = ada;
    (
        RecordService::new(doc, host, "ada"),
        packages[0].meta.clone(),
    )
}

fn action_id(meta: &tilolang_runtime::Meta, name: &str) -> String {
    meta.actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the action `{name}`"))
        .id
        .clone()
}

fn args(value: Json) -> Map<String, Json> {
    value.as_object().cloned().unwrap()
}

/// A new note, made through the notes app's `new_note` on the notebook the session publishes.
fn new_note(service: &mut RecordService, meta: &tilolang_runtime::Meta) -> String {
    let notebook = service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{APP}.Notes"))
        .expect("the notebook is published");
    let started = service
        .invoke(
            notebook.id.as_str(),
            &action_id(meta, "new_note"),
            &Map::new(),
        )
        .unwrap();
    let Started::Done(output) = started else {
        panic!("a tilolang action is done once committed");
    };
    service.settle();
    output.resources.first().cloned().expect("the note it made")
}

/// Every text a tree shows, in its props.
fn texts(node: &UiNode) -> Vec<String> {
    fn walk(node: &UiNode, out: &mut Vec<String>) {
        for value in node.props.values() {
            if let Value::Text(text) = value {
                out.push(text.clone());
            }
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

#[test]
fn a_note_made_through_its_action_is_a_session_resource_with_its_fields() {
    let (mut service, meta) = service();
    let note = new_note(&mut service, &meta);
    assert!(
        note.starts_with(&resource_id(APP, "")),
        "the note is named by its published id: {note}"
    );
    assert!(
        service.resources().iter().any(|resource| {
            resource
                .children
                .iter()
                .any(|child| child.id.as_str() == note)
        }),
        "the note stands in the published catalogue"
    );

    service.poll();
    service
        .invoke(
            &note,
            &action_id(&meta, "retitle"),
            &args(json!({ "title": "Groceries" })),
        )
        .unwrap();
    assert!(service.poll(), "a retitled note moves its reading");
    let reading = service.read(&note).expect("the note reads");
    assert_eq!(
        reading[field_id(&meta, "Note", "title")],
        json!("Groceries")
    );
    assert_eq!(reading[field_id(&meta, "Note", "pinned")], json!(false));

    service
        .invoke(&note, &action_id(&meta, "pin"), &Map::new())
        .unwrap();
    assert!(
        service.poll(),
        "a field no published tree shows moves the reading too"
    );
    let reading = service.read(&note).unwrap();
    assert_eq!(reading[field_id(&meta, "Note", "pinned")], json!(true));
    assert!(!service.poll(), "nothing changed since");
}

#[test]
fn another_app_reads_the_published_note_through_the_notes_package() {
    let (mut service, meta) = service();
    let note = new_note(&mut service, &meta);
    service
        .invoke(
            &note,
            &action_id(&meta, "retitle"),
            &args(json!({ "title": "Groceries" })),
        )
        .unwrap();
    let reading = service.read(&note).unwrap();

    // The shell's side: notes is another app's package here, its records read from the session.
    let shell = Host::new(&AutoCommit::new(), Principal::new("ada"), || None).unwrap();
    let notes = tilolang::compile_package(NOTES.0, &IdsLock::parse(NOTES.1).unwrap(), &[])
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    shell
        .load_foreign_package(&notes.wasm, &notes.meta)
        .unwrap();
    let card = tilolang::compile_package(CARD, &IdsLock::new(), std::slice::from_ref(&notes.meta))
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    shell.load_compiled(&card).unwrap();
    let kind = service
        .resources()
        .iter()
        .flat_map(|resource| resource.children.iter())
        .find(|child| child.id.as_str() == note)
        .map(|child| child.kind.clone())
        .expect("the note is published");
    let ty = reading_type(&kind).expect("a tilolang record's kind");
    assert_eq!(ty, format!("{APP}#Note"));
    let target = ResourceRef::parse_id(&note, ty).expect("a resource id");
    shell
        .runtime()
        .set_reading(&target, &reading.to_string())
        .unwrap();
    let card_view = card
        .meta
        .views
        .iter()
        .find(|view| view.name == "note_card")
        .unwrap()
        .id
        .clone();
    let view = shell.open_resource(&target, Some(&card_view)).unwrap();
    assert!(
        texts(view.tree()).contains(&"Groceries".to_owned()),
        "the notes app's row shows the published note: {:#?}",
        view.tree()
    );
}
