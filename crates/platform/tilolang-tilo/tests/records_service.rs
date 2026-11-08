//! A workspace's tilolang records as session resources: a note made through the notes app's own
//! action is published, its reading is what a runtime loading notes as another app's package
//! reads back, and a change to the note moves the reading.

use automerge::AutoCommit;
use serde_json::{Map, Value as Json, json};
use tilo_service::{Service, Started};
use tilolang::IdsLock;
use tilolang_runtime::{Principal, ResourceRef, UiNode, Value};
use tilolang_tilo::host::Host;
use tilolang_tilo::notices::{Notice, Notices, Tell};
use tilolang_tilo::publish::resource_id;
use tilolang_tilo::records_service::{RecordService, reading_type};

use crate::fixtures::{Replica, field_id};

const DISCUSSION: (&str, &str) = (
    include_str!("../../../../apps/tl/discussion/app.tl"),
    include_str!("../../../../apps/tl/discussion/app.ids"),
);
const NOTES: (&str, &str) = (
    include_str!("../../../../apps/tl/notes/app.tl"),
    include_str!("../../../../apps/tl/notes/app.ids"),
);
const TASKS: (&str, &str) = (
    include_str!("../../../../apps/tl/tasks/app.tl"),
    include_str!("../../../../apps/tl/tasks/app.ids"),
);
const APP: &str = "so.tilo.lang.notes";
const TASKS_APP: &str = "so.tilo.lang.tasks";

/// A view of another app's package that shows a note by the notes app's own row view.
const CARD: &str = r#"app test.card "Card"

use so.tilo.lang.notes as notes

view note_card(n: notes.Note) {
  notes.note_row(n)
}
"#;

/// Ada's workspace with its notebook, published.
fn service() -> (RecordService, tilolang_runtime::Meta) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[DISCUSSION, NOTES]);
    ada.host.root_of(APP).unwrap();
    ada.commit();
    let Replica { doc, host, .. } = ada;
    (
        RecordService::new(doc, host, "ada"),
        packages.last().unwrap().meta.clone(),
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
        .find(|resource| resource.kind == format!("tl.{APP}.Notebook"))
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
    let discussion =
        tilolang::compile_package(DISCUSSION.0, &IdsLock::parse(DISCUSSION.1).unwrap(), &[])
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    let notes = tilolang::compile_package(
        NOTES.0,
        &IdsLock::parse(NOTES.1).unwrap(),
        std::slice::from_ref(&discussion.meta),
    )
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

/// Ada's workspace with its tasks, as the session runs them: a `RecordService` over the record,
/// with no window of the app open, and the clock its app and its triggers read.
fn tasks() -> (
    RecordService,
    tilolang_runtime::Meta,
    std::rc::Rc<std::cell::RefCell<String>>,
) {
    let clock = std::rc::Rc::new(std::cell::RefCell::new("2026-10-27T09:00:00Z".to_owned()));
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[DISCUSSION, TASKS]);
    ada.host.set_clock({
        let clock = std::rc::Rc::clone(&clock);
        move || clock.borrow().clone()
    });
    ada.host.root_of(TASKS_APP).unwrap();
    ada.commit();
    let Replica { doc, host, .. } = ada;
    let mut service = RecordService::new(doc, host, "ada");
    service.set_clock({
        let clock = std::rc::Rc::clone(&clock);
        move || clock.borrow().clone()
    });
    (service, packages.last().unwrap().meta.clone(), clock)
}

/// The id of the app's published root: the tasks.
fn tasks_root(service: &RecordService) -> tilo_actions::ResourceId {
    service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{TASKS_APP}.Tasks"))
        .expect("the tasks are published")
        .id
}

/// What the packages told, as the notifications service would have shown it: the recording
/// stand-in for the person's notifications.
#[derive(Default)]
struct Told(std::rc::Rc<std::cell::RefCell<Vec<Notice>>>);

impl Tell for Told {
    fn show(&self, notice: &Notice) {
        self.0.borrow_mut().push(notice.clone());
    }

    fn withdraw(&self, _tag: &str) {}
}

/// A task added by `new_task` under `title`.
fn task(service: &mut RecordService, meta: &tilolang_runtime::Meta, title: &str) -> String {
    let root = tasks_root(service);
    let started = service
        .invoke(
            root.as_str(),
            &action_id(meta, "new_task"),
            &args(json!({ "title": title })),
        )
        .unwrap();
    let Started::Done(output) = started else {
        panic!("a tilolang action is done once committed");
    };
    service.settle();
    output.resources.first().cloned().expect("the task it made")
}

/// The service keeps every loaded package's records, so a launch of any of them is hosted: the
/// address it names is the resource the app's views read their state from (its root), by the id
/// the door publishes it at (`docs/architecture/apps.md` §8 step 2, `one-app-model.md` §4).
/// Whether a launch is hosted follows from the service keeping the record, never from per-app
/// code.
#[test]
fn the_service_names_the_root_of_every_app_whose_record_it_keeps() {
    let (service, meta) = service();
    // The notes app's record is the notebook: the root the service publishes for it, by the
    // root type's id (`root-<type id>`, as `Host::root_record` makes it).
    let notebook = meta.roots.first().expect("the notes declare their root");
    assert_eq!(
        service.hosted(APP).as_deref(),
        Some(resource_id(APP, &format!("root-{notebook}")).as_str()),
        "the address is the app's root, as the door publishes it"
    );
    // A package this service loaded is hosted as one; an app it keeps nothing of is not, and
    // its window opens its own record as every app did before the session served one.
    assert_eq!(service.hosted("so.tilo.nowhere"), None);
}

#[test]
fn a_reminder_writes_the_notice_of_its_person_at_its_moment_once() {
    let (mut service, meta, clock) = tasks();
    let told: std::rc::Rc<std::cell::RefCell<Vec<Notice>>> = std::rc::Rc::default();
    service.set_tell(Told(std::rc::Rc::clone(&told)));

    // Ada adds a task, gives it to Bo, and asks to be reminded of it in an hour.
    let call = task(&mut service, &meta, "Call the landlord");
    service
        .invoke(
            &call,
            &action_id(&meta, "assign"),
            &args(json!({ "who": { "person": "bo" } })),
        )
        .unwrap();
    service
        .invoke(
            &call,
            &action_id(&meta, "remind_at"),
            &args(json!({ "when": "2026-10-27T10:00:00Z" })),
        )
        .unwrap();
    assert_eq!(
        service.wakes_in(),
        Some(std::time::Duration::from_secs(3600)),
        "the workspace wakes for the reminder, not on a poll"
    );

    // Bo's device is there before the moment, having looked: nothing yet.
    let mut theirs = Notices::new("bo");
    assert!(theirs.step(service.host()).show.is_empty());

    // A second short of the moment: nothing is written.
    *clock.borrow_mut() = "2026-10-27T09:59:59Z".to_owned();
    service.poll();
    assert!(notices(service.host()).is_empty(), "not before its moment");

    // At the moment the reminder writes Bo's notice, in the same change as the task's own: the
    // person it names, what it says, and the task it opens.
    *clock.borrow_mut() = "2026-10-27T10:00:00Z".to_owned();
    assert!(service.poll(), "the trigger ran");
    let held = notices(service.host());
    assert_eq!(held.len(), 1, "one notice: {held:#?}");
    assert_eq!(held[0].0, "bo", "whoever is doing it");
    assert_eq!(held[0].1, "Call the landlord");
    assert_eq!(
        held[0].2, "",
        "a task with no due day and no project says no more"
    );
    assert_eq!(held[0].3, call, "the task the notice opens");

    // Bo's device shows it; Ada's, at this one, does not.
    let theirs = theirs.step(service.host());
    assert_eq!(
        theirs.show.len(),
        1,
        "the person it names sees it: {theirs:#?}"
    );
    assert_eq!(theirs.show[0].opens.app, TASKS_APP);
    assert_eq!(theirs.show[0].opens.resource, call);
    assert!(told.borrow().is_empty(), "this device is Ada's");

    // And once: the turn after tells nothing of it again.
    service.poll();
    assert_eq!(
        notices(service.host()).len(),
        1,
        "written once, not every turn"
    );
}

/// The notices the record holds: each one's person, what it says, and the resource it opens.
fn notices(host: &Host) -> Vec<(String, String, String, String)> {
    host.store()
        .resources_of(tilolang::HOST_APP, tilolang::NOTICE_TYPE)
        .into_iter()
        .filter_map(|id| {
            let at = ResourceRef::root(id, tilolang::NOTICE_TYPE.to_owned());
            let to = match host.runtime().read(&at, tilolang::NOTICE_TO).ok()? {
                Value::Principal(person) => person.0,
                _ => return None,
            };
            let Value::Text(title) = host.runtime().read(&at, tilolang::NOTICE_TITLE).ok()? else {
                return None;
            };
            let body = match host.runtime().read(&at, tilolang::NOTICE_BODY).ok()? {
                Value::Text(text) => text,
                _ => String::new(),
            };
            let Value::Ref(reference) = host.runtime().read(&at, tilolang::NOTICE_OPENS).ok()?
            else {
                return None;
            };
            let target = &reference.target;
            let app = &tilolang_tilo::schema::lookup(&target.ty)?.app;
            let opens = tilolang_tilo::publish::resource_id(app, &target.id_string());
            Some((to, title, body, opens))
        })
        .collect()
}
