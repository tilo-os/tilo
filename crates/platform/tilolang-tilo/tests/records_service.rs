//! A workspace's tilolang records as session resources: a note made through the notes app's own
//! action is published, its reading is what a runtime loading notes as another app's package
//! reads back, and a change to the note moves the reading. A change too large to record whole
//! keeps its large parts in the record's content store, which the session lends
//! ([`RecordService::provide_content`]), so an agent's paste is recorded and reads back here and
//! on a peer.

use automerge::AutoCommit;
use serde_json::{Map, Value as Json, json};
use tilo_service::{Service, Started};
use tilo_session_core::notice::Notice;
use tilolang::IdsLock;
use tilolang_runtime::{Principal, ResourceRef, UiNode, Value};
use tilolang_tilo::host::Host;
use tilolang_tilo::notices::Notices;
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
    crate::fixtures::TASKS_SOURCE,
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

fn args(value: Json) -> Map<String, Json> {
    value.as_object().cloned().unwrap()
}

/// A new note, made through the notes app's `new_note` on the notebook the session publishes.
fn new_note(service: &mut RecordService) -> String {
    let notebook = service
        .resources()
        .into_iter()
        .find(|resource| resource.kind == format!("tl.{APP}.Notebook"))
        .expect("the notebook is published");
    let started = service
        .invoke(notebook.id.as_str(), "new_note", &Map::new())
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

/// A published resource's own `open` is answered, never refused: opening is the system's
/// (`docs/designs/app-language/one-app-model.md` §3; shell.md §8, "The canvas composes views; it
/// opens nothing"). The record publishes the `open` so an agent reading the catalogue finds it,
/// and invoking it changes nothing of the record — the place it asks for waits for the session to
/// stand through its arrangement, beside the place the asking step was in.
#[test]
fn a_resources_own_open_is_answered_and_changes_no_record() {
    let (mut service, _) = service();
    let note = new_note(&mut service);
    // The note publishes its own `open`: the effect a search hit and an agent invoke.
    let published = service
        .resources()
        .into_iter()
        .flat_map(|resource| std::iter::once(resource.clone()).chain(resource.children))
        .find(|resource| resource.id.as_str() == note)
        .expect("the note is published");
    let open = published
        .actions
        .iter()
        .find(|action| action.id == "open")
        .expect("the note publishes its own open");
    assert!(open.encode_immediate().is_some(), "its open takes no input");

    let before = service.record().clone().get_heads();
    // Invoked as an agent answering in a conversation, the open is answered with nothing done to
    // the record: the session places the note beside that conversation.
    let agent =
        tilo_actions::ActionInvoker::actor("agent:tide").with_kind(tilo_actions::ActorKind::Agent);
    service.note_beside("chat.workspace/channels/c_tide");
    let started = service
        .invoke_as(&agent, &note, "open", &Map::new())
        .unwrap_or_else(|error| panic!("the resource's open is answered: {error}"));
    assert_eq!(
        started,
        Started::Done(tilo_actions::ActionOutput::default()),
        "it changed nothing of the record"
    );
    assert_eq!(
        service.record().clone().get_heads(),
        before,
        "the record did not change"
    );
    // What it asked stands for the session to place: the note, beside the conversation that asked,
    // and the actor that asked is the agent's.
    let opened = service.take_open();
    assert_eq!(opened.len(), 1, "one place asked for");
    let (invoker, place) = &opened[0];
    assert_eq!(invoker.actor, "agent:tide", "asked as the agent");
    assert_eq!(place.resource, note, "the note the invocation addressed");
    assert_eq!(place.view, "", "its primary view: its own default `open`");
    assert_eq!(
        place.beside, "chat.workspace/channels/c_tide",
        "beside the conversation of the turn that asked"
    );
    // Nothing is left to take: the place asked for once.
    assert!(service.take_open().is_empty(), "the ask is taken once");
}

#[test]
fn a_note_made_through_its_action_is_a_session_resource_with_its_fields() {
    let (mut service, meta) = service();
    let note = new_note(&mut service);
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
        .invoke(&note, "retitle", &args(json!({ "title": "Groceries" })))
        .unwrap();
    assert!(service.poll(), "a retitled note moves its reading");
    let reading = service.read(&note).expect("the note reads");
    assert_eq!(
        reading[field_id(&meta, "Note", "title")],
        json!("Groceries")
    );
    assert_eq!(reading[field_id(&meta, "Note", "pinned")], json!(false));

    service.invoke(&note, "pin", &Map::new()).unwrap();
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
    let (mut service, _) = service();
    let note = new_note(&mut service);
    service
        .invoke(&note, "retitle", &args(json!({ "title": "Groceries" })))
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
fn tasks() -> (RecordService, std::rc::Rc<std::cell::RefCell<String>>) {
    let clock = std::rc::Rc::new(std::cell::RefCell::new("2026-10-27T09:00:00Z".to_owned()));
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[DISCUSSION, TASKS]);
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
    (service, clock)
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

impl tilo_session_core::notice::Notices for Told {
    fn show(&self, notice: &Notice) {
        self.0.borrow_mut().push(notice.clone());
    }

    fn withdraw(&self, _tag: &str) {}
}

/// A task added by `new_task` under `title`.
fn task(service: &mut RecordService, title: &str) -> String {
    let root = tasks_root(service);
    let started = service
        .invoke(root.as_str(), "new_task", &args(json!({ "title": title })))
        .unwrap();
    let Started::Done(output) = started else {
        panic!("a tilolang action is done once committed");
    };
    service.settle();
    output.resources.first().cloned().expect("the task it made")
}

#[test]
fn a_reminder_writes_the_notice_of_its_person_at_its_moment_once() {
    let (mut service, clock) = tasks();
    let told: std::rc::Rc<std::cell::RefCell<Vec<Notice>>> = std::rc::Rc::default();
    service.set_tell(Told(std::rc::Rc::clone(&told)));

    // Ada adds a task, gives it to Bo, and asks to be reminded of it in an hour.
    let call = task(&mut service, "Call the landlord");
    service
        .invoke(&call, "assign", &args(json!({ "who": { "person": "bo" } })))
        .unwrap();
    service
        .invoke(
            &call,
            "remind_at",
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
    let opens = theirs.show[0]
        .place
        .as_ref()
        .expect("it opens what it names");
    assert_eq!(opens.app, TASKS_APP);
    assert_eq!(opens.resource, call);
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

/// A body past a mebibyte, a paragraph a line: what an agent pastes into a note.
fn pasted_body() -> String {
    use std::fmt::Write as _;
    let mut body = String::new();
    for at in 0..17 {
        let _ = writeln!(body, "Pasted paragraph {at}: {}", "words ".repeat(11_000));
    }
    body
}

/// The change of `doc` whose recorded action keeps parts as content, and those parts.
fn kept_as_content(doc: &mut tilo_resource::Doc) -> (automerge::ChangeHash, Vec<String>) {
    let document = doc.document();
    document
        .get_changes(&[])
        .iter()
        .rev()
        .find_map(|change| {
            let hash = change.hash();
            let action = tilo_actions::entry_of(document, hash)?.action?;
            let parts = action.content_parts();
            (!parts.is_empty()).then_some((hash, parts))
        })
        .expect("the paste's change keeps its body as content")
}

/// The address a sealed reference names its ciphertext by.
fn address(reference: &str) -> &str {
    reference
        .split_once('.')
        .map_or(reference, |(hash, _)| hash)
}

/// A note whose body is past a mebibyte, written by an agent through the records service, is kept
/// as content in the record's store: the paste is recorded rather than refused, the note reads it
/// back, and the change decodes from the store alone — after the record opens again, and on a
/// peer that asks the record's route for its parts.
#[test]
fn an_agents_paste_past_a_mebibyte_is_kept_and_reads_back_here_and_on_a_peer() {
    let (mut service, _) = service();
    let store = tilo_content::ContentStore::memory().sealing();
    service.provide_content(APP, store.clone());
    let note = new_note(&mut service);
    let body = pasted_body();
    let agent =
        tilo_actions::ActionInvoker::actor("agent:tide").with_kind(tilo_actions::ActorKind::Agent);
    let started = service
        .invoke_as(
            &agent,
            &note,
            tilolang_tilo::action::EDIT_TEXT_ACTION,
            &args(json!({"field": "body", "value": body})),
        )
        .expect("the paste is recorded rather than refused");
    assert!(matches!(started, Started::Done(_)), "the paste is done");
    service.settle();
    let reading = serde_json::to_string(&service.read(&note).expect("the note is published"))
        .expect("the reading reads");
    assert!(
        reading.contains("Pasted paragraph 16:"),
        "the note reads the paste back: {reading:.200}"
    );

    // Its change keeps the body as content, sealed, in the record's store.
    let (paste, parts) = kept_as_content(&mut service.record().clone());
    assert!(
        parts.iter().all(|part| tilo_content::is_sealed(part)),
        "a record's parts are sealed: {parts:?}"
    );
    assert!(
        parts.iter().all(|part| store.has(part)),
        "the record's store keeps every part: {parts:?}"
    );

    // The record opens again over what the device holds: the change decodes from the store alone,
    // and the note the reopened record publishes reads the paste back.
    let reopened = reopened_service(service.record(), &store);
    assert_eq!(
        crate::window::decoded(reopened.record().clone(), paste, store.clone()).unwrap(),
        tilo_actions::Verified::Reproduced,
        "after the record opens again, the paste reads back as it ran"
    );
    let reading = serde_json::to_string(&reopened.read(&note).expect("the note is published"))
        .expect("the reading reads");
    assert!(
        reading.contains("Pasted paragraph 16:"),
        "the reopened record reads the paste back: {reading:.200}"
    );

    // A peer holds the change and not its parts: decoding asks the record's route, and once what
    // was offered there arrives, the action reproduces its change there too.
    let lane = std::sync::Arc::new(collaboration::record_content::ContentLane::default());
    let theirs = collaboration::record_content::RecordContent::store(
        tilo_content::ContentStore::memory(),
        std::sync::Arc::clone(&lane),
    );
    assert!(
        matches!(
            crate::window::decoded(reopened.record().clone(), paste, theirs.clone()).unwrap(),
            tilo_actions::Verified::ContentMissing(_)
        ),
        "the peer lacks the paste until it arrives"
    );
    let wanted: Vec<String> = lane.take_requests().wants;
    assert!(
        parts
            .iter()
            .any(|part| wanted.iter().any(|hash| hash == address(part))),
        "the peer asks the record's route for the paste: wanted {wanted:?}"
    );
    for part in &parts {
        let hash = address(part);
        let bytes = store
            .read(hash)
            .expect("the record's store keeps the ciphertext");
        lane.arrive(hash.to_owned(), bytes);
    }
    assert_eq!(
        crate::window::decoded(reopened.record().clone(), paste, theirs).unwrap(),
        tilo_actions::Verified::Reproduced,
        "once the paste arrived, the peer's action reproduces its change"
    );
}

/// The record `doc` opened again, its content kept in `store`: a fresh service over the document
/// the device holds, as the session opens it on the next start.
fn reopened_service(doc: &tilo_resource::Doc, store: &tilo_content::ContentStore) -> RecordService {
    let (replica, _) = Replica::new("ada", doc.clone(), &[DISCUSSION, NOTES]);
    let Replica { doc, host, .. } = replica;
    let mut service = RecordService::new(doc, host, "ada");
    service.provide_content(APP, store.clone());
    service.settle();
    service
}
