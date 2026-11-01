//! A place on the shell's canvas that names another app's record composes that app's own view
//! over the record's reading as the session publishes it, and an edit in the composed view
//! goes to the record's door: the note the notes app reads is the note the place shows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures"
)]

use serde_json::{Map, Value as Json, json};
use tilo_service::{Service as _, Started};
use tilolang_tilo::records_service::RecordService;

use super::tests_support::{part_with, reading_with_windows};
use super::*;

const NOTES_APP: &str = "so.tilo.lang.notes";

/// A room's notebook, published as the session publishes it.
fn notebook() -> (RecordService, tilolang_tilo::runtime::Meta) {
    let record = tilo_resource::Doc::new();
    let host = Host::new(&record, Principal::new("Pat"), || None).unwrap();
    let notes = host
        .load_builtin(
            include_str!("../../../../../apps/tl/notes/app.tl"),
            include_str!("../../../../../apps/tl/notes/app.ids"),
        )
        .unwrap();
    host.root_of(NOTES_APP).unwrap();
    let mut record = record;
    for committed in host.take_committed() {
        tilo_actions_apply(&host, &mut record, &committed.action);
    }
    (RecordService::new(record, host, "Pat"), notes.meta)
}

/// Commit what the runtime made on its own (the notebook's root) to `record`.
fn tilo_actions_apply(
    host: &Host,
    record: &mut tilo_resource::Doc,
    action: &tilolang_tilo::DynAction,
) {
    let context = ui::tilo_actions::ActionContext::for_resource("room", &action.kind, &action.kind)
        .with_invoker(ui::tilo_actions::ActionInvoker::actor("Pat"));
    host.with_schemas(|| {
        ui::tilo_actions::apply_local_with_context(
            record,
            action,
            action.encode().unwrap(),
            Some(b"Pat"),
            context,
        )
    })
    .unwrap();
    record.commit();
    host.sync_from(record);
}

fn action_id(meta: &tilolang_tilo::runtime::Meta, name: &str) -> String {
    meta.actions
        .iter()
        .find(|action| action.name == name)
        .unwrap()
        .id
        .clone()
}

fn field_id(meta: &tilolang_tilo::runtime::Meta, ty: &str, field: &str) -> String {
    meta.types
        .iter()
        .find(|declared| declared.name == ty)
        .and_then(|declared| declared.fields.iter().find(|f| f.name == field))
        .unwrap()
        .id
        .clone()
}

/// A note titled `title`, made through the notes app's own actions.
fn note(service: &mut RecordService, meta: &tilolang_tilo::runtime::Meta, title: &str) -> String {
    let notebook = service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{NOTES_APP}.Notes"))
        .unwrap();
    let Started::Done(made) = service
        .invoke(
            notebook.id.as_str(),
            &action_id(meta, "new_note"),
            &Map::new(),
        )
        .unwrap()
    else {
        panic!("a tilolang action is done once committed");
    };
    let note = made.resources.first().cloned().unwrap();
    service
        .invoke(
            &note,
            &action_id(meta, "retitle"),
            json!({ "title": title }).as_object().unwrap(),
        )
        .unwrap();
    service.settle();
    note
}

/// The person's arrangement: one placed strip on the canvas, its one place showing `record` by
/// the notes app's editor.
fn arrangement(record: &str) -> ArrangementReading {
    ArrangementReading {
        workspaces: vec![ArrangementWorkspaceReading {
            key: "w1".to_owned(),
            name: "Desk".to_owned(),
        }],
        rows: vec![ArrangementRowReading {
            key: "r1".to_owned(),
            workspace: "w1".to_owned(),
            name: "Main".to_owned(),
            shell: String::new(),
        }],
        strips: vec![ArrangementStripReading {
            key: "s1".to_owned(),
            workspace: "w1".to_owned(),
            row: "r1".to_owned(),
            width: 1000,
            placed: true,
            canvas_width: 1100,
            canvas_height: 1200,
            ..ArrangementStripReading::default()
        }],
        places: vec![ArrangementPlaceReading {
            key: "p1".to_owned(),
            strip: "s1".to_owned(),
            shows: "Note".to_owned(),
            shown_as: "note_editor".to_owned(),
            record: record.to_owned(),
            ..ArrangementPlaceReading::default()
        }],
    }
}

/// The shell's spaces canvas over the arrangement the part holds now.
fn canvas(part: &ShellPart) -> tilolang_tilo::runtime::ViewInstance {
    let host = part.instance.host().expect("the shell runs");
    let app = host
        .packages()
        .last()
        .map(|package| package.meta.app.clone())
        .unwrap();
    let root = host
        .root_of_layer(
            &app,
            &RootView {
                root: Some("Arrangement".to_owned()),
                view: None,
            },
        )
        .unwrap();
    let view = host
        .package(&app)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == "spaces_canvas")
        .unwrap()
        .id
        .clone();
    host.open_resource(&root, Some(&view)).unwrap()
}

/// The text input bound to the field `field` of a resource of another app.
fn input_bound_to<'a>(
    node: &'a tilolang_tilo::runtime::UiNode,
    field: &str,
) -> Option<&'a tilolang_tilo::runtime::UiNode> {
    let bound = node.binding.as_ref().is_some_and(|binding| {
        matches!(
            &binding.target,
            tilolang_tilo::runtime::view::BindTarget::Field { target, field: bound }
                if bound == field && target.ty.contains('#')
        )
    });
    if node.kind == "ui.TextInput" && bound {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| input_bound_to(child, field))
}

/// Every text the tree shows, in its props.
fn texts(node: &tilolang_tilo::runtime::UiNode) -> Vec<String> {
    let mut out: Vec<String> = node
        .props
        .values()
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect();
    for child in &node.children {
        out.extend(texts(child));
    }
    out
}

#[test]
fn a_place_composes_the_notes_editor_over_the_note_and_an_edit_there_changes_the_note() {
    let (mut service, meta) = notebook();
    let note = note(&mut service, &meta, "Groceries");

    let (mut part, link) = part_with(reading_with_windows());
    let arrangement = arrangement(&note);
    link.publish_arrangement(arrangement.clone());
    assert!(
        link.follow_places(&arrangement, &service.resources(), |id| service.read(id)),
        "the place's record has a reading"
    );
    for _ in 0..4 {
        part.prepare_frame();
    }

    let view = canvas(&part);
    let title_field = field_id(&meta, "Note", "title");
    assert!(
        texts(view.tree()).contains(&"Groceries".to_owned()),
        "the place shows the note: {:#?}",
        view.tree()
    );
    let title = input_bound_to(view.tree(), &title_field)
        .unwrap_or_else(|| panic!("the place shows the note's editor: {:#?}", view.tree()));
    assert_eq!(
        view.edit(&title.key, &Value::text("Groceries and lemons"))
            .unwrap(),
        tilolang_tilo::runtime::EditOutcome::Applied
    );
    assert!(link.has_asks(), "the edit is asked of the note's door");

    // The edit went to the note's door, as the session's turn runs what the shell asked.
    link.serve(|ask| {
        let arguments: Map<String, Json> = ask
            .arguments
            .iter()
            .map(|(name, value)| (name.clone(), Json::String(value.clone())))
            .collect();
        service
            .invoke(&ask.resource, &ask.action, &arguments)
            .map(drop)
            .map_err(|error| panic!("the note's door refused {ask:?}: {error:?}"))
    });
    assert_eq!(
        service.read(&note).unwrap()[&title_field],
        json!("Groceries and lemons"),
        "the note the notes app reads changed"
    );

    // And the place draws the note as it is now.
    assert!(link.follow_places(&arrangement, &service.resources(), |id| service.read(id)));
    for _ in 0..4 {
        part.prepare_frame();
    }
    let view = canvas(&part);
    assert!(
        texts(view.tree()).contains(&"Groceries and lemons".to_owned()),
        "{:#?}",
        view.tree()
    );
}

#[test]
fn the_shell_keeps_no_copy_of_the_apps_it_composes() {
    let (part, _link) = part_with(reading_with_windows());
    let host = part.instance.host().expect("the shell runs");
    // The apps the shell's views compose are other apps' here: what the shell's runtime runs of
    // them reads their records from the session, and the shell's record holds none of theirs.
    for app in ["so.tilo.lang.chat", NOTES_APP, "so.tilo.lang.tasks"] {
        assert!(
            host.package(app).is_none(),
            "`{app}` runs as the shell's own package, keeping a copy of its records"
        );
    }
    assert_eq!(
        host.packages()
            .iter()
            .map(|package| package.meta.app.clone())
            .collect::<Vec<_>>(),
        vec![host.packages().last().unwrap().meta.app.clone()],
        "the shell runs its own package alone"
    );
}
