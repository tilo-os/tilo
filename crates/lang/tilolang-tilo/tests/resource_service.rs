//! A workspace's tilolang resources as session resources: a note made through the notes app's own
//! action is published, its reading is what a runtime loading notes as another app's package
//! reads back, and a change to the note moves the reading.

use crate::fixtures::Workspace;
use serde_json::{Map, Value as Json, json};
use tilo_service::notice::Notice;
use tilo_service::{Service, Started};
use tilolang::IdsLock;
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value};
use tilolang_tilo::host::Host;
use tilolang_tilo::notices::Notices;
use tilolang_tilo::publish::resource_id;
use tilolang_tilo::resource_service::{ResourceService, reading_type};

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
const APP: &str = "so.tilo.notes";
const TASKS_APP: &str = "so.tilo.tasks";

/// A view of another app's package that shows a note by the notes app's own row view.
const CARD: &str = r#"app test.card "Card"

use so.tilo.notes as notes

view note_card(n: notes.Note) {
  notes.note_row(n)
}
"#;

/// Ada's workspace with its notebook, published.
fn service() -> (ResourceService, tilolang_runtime::Meta) {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[DISCUSSION, NOTES]);
    ada.host.root_of(APP).unwrap();
    ada.commit();
    (
        ada.into_service("ada"),
        packages.last().unwrap().meta.clone(),
    )
}

fn args(value: Json) -> Map<String, Json> {
    value.as_object().cloned().unwrap()
}

/// A new note, made through the notes app's `new_note` on the notebook the session publishes.
fn new_note(service: &mut ResourceService) -> String {
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

#[test]
fn an_action_through_the_service_is_written_as_one_change() {
    let (mut service, _) = service();
    let watch = service.host().store().watch();
    new_note(&mut service);
    let moved = watch.take();
    assert_eq!(
        moved.docs.len(),
        1,
        "the action's plan wrote one document: {moved:?}"
    );
}

/// Every text a tree shows, in its props.
fn texts(node: &UiNode) -> Vec<String> {
    crate::fixtures::nodes(node)
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

tilo_test_env::scenario! {
    fn the_resource_service_answers_for_a_packages_resources;

    phase a_resources_own_open_is_answered_and_changes_no_document;
    // A published resource's own `open` is answered, never refused: opening is the system's
    // (`docs/designs/app-language/one-app-model.md` §3; shell.md §8, "The canvas composes views; it
    // opens nothing"). The document publishes the `open` so an agent reading the catalogue finds it,
    // and invoking it changes nothing of the document — the place it asks for waits for the session to
    // stand through its layout, beside the place the asking step was in.
    {
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

        let watch = service.host().store().watch();
        // A new watch says everything moved until it is first taken.
        let _ = watch.take();
        // Invoked as an agent answering in a conversation, the open is answered with nothing done to
        // the document: the session places the note beside that conversation.
        let agent =
            tilo_actions::ActionInvoker::agent("tide");
        service.note_beside("chat.workspace/channels/c_tide");
        let started = service
            .invoke_as(&agent, &note, "open", &Map::new())
            .unwrap_or_else(|error| panic!("the resource's open is answered: {error}"));
        assert_eq!(
            started,
            Started::Done(tilo_actions::ActionOutput::default()),
            "it changed nothing of the document"
        );
        assert!(
            watch.take().is_empty(),
            "the store did not change"
        );
        // What it asked stands for the session to place: the note, beside the conversation that asked,
        // and the actor that asked is the agent's.
        let opened = service.take_open();
        assert_eq!(opened.len(), 1, "one place asked for");
        let (invoker, place) = &opened[0];
        assert_eq!(
            invoker.acting.by,
            tilolang_tilo::runtime::ids::MemberId::agent("tide").into(),
            "asked as the agent"
        );
        assert_eq!(place.resource, note, "the note the invocation addressed");
        assert_eq!(place.view, "", "its primary view: its own default `open`");
        assert_eq!(
            place.beside, "chat.workspace/channels/c_tide",
            "beside the conversation of the turn that asked"
        );
        // Nothing is left to take: the place asked for once.
        assert!(service.take_open().is_empty(), "the ask is taken once");
    }

    phase a_note_made_through_its_action_is_a_session_resource_with_its_fields;
    {
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

    phase another_app_reads_the_published_note_through_the_notes_package;
    {
        let (mut service, _) = service();
        let note = new_note(&mut service);
        service
            .invoke(&note, "retitle", &args(json!({ "title": "Groceries" })))
            .unwrap();
        let reading = service.read(&note).unwrap();

        // The shell's side: notes is another app's package here, its documents read from the session.
        let shell = Host::new(Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")), || None).unwrap();
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
        let ty = reading_type(&kind).expect("a tilolang document's kind");
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
}

/// Ada's workspace with its tasks, as the session runs them: a `ResourceService` over the document,
/// with no window of the app open, and the clock its app and its triggers read.
fn tasks() -> (ResourceService, std::rc::Rc<std::cell::RefCell<String>>) {
    let clock = std::rc::Rc::new(std::cell::RefCell::new("2026-10-27T09:00:00Z".to_owned()));
    let (ada, _) = Replica::new("ada", Workspace::new(), &[DISCUSSION, TASKS]);
    ada.host.set_clock({
        let clock = std::rc::Rc::clone(&clock);
        move || clock.borrow().clone()
    });
    ada.host.root_of(TASKS_APP).unwrap();
    ada.commit();
    let mut service = ada.into_service("ada");
    service.set_clock({
        let clock = std::rc::Rc::clone(&clock);
        move || clock.borrow().clone()
    });
    (service, clock)
}

/// The id of the app's published root: the tasks.
fn tasks_root(service: &ResourceService) -> tilo_actions::ResourceId {
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

impl tilo_service::notice::Notices for Told {
    fn show(&self, notice: &Notice) {
        self.0.borrow_mut().push(notice.clone());
    }

    fn withdraw(&self, _tag: &str) {}
}

/// A task added by `new_task` under `title`.
fn task(service: &mut ResourceService, title: &str) -> String {
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

/// The notices the document holds: each one's person, what it says, and the resource it opens.
fn notices(host: &Host) -> Vec<(String, String, String, String)> {
    host.store()
        .resources_of(tilolang::NOTICE_TYPE)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| {
            let at = ResourceRef::root(id, tilolang::NOTICE_TYPE.to_owned());
            let to = match host.runtime().read(&at, tilolang::NOTICE_TO).ok()? {
                Value::Actor(person) => person.id().to_owned(),
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

/// An action that sets what its document holds already changes nothing and is done, as any caller
/// reads it: a program that moved a task to Friday and then set its reminder to the moment the
/// move gave it failed on the second ("the document holds no writes of this actor to commit").
#[test]
fn an_action_that_writes_what_is_there_is_done() {
    let (mut service, _) = tasks();
    let made = task(&mut service, "Call mum");
    for _ in 0..2 {
        let started = service
            .invoke(
                made.as_str(),
                "remind_at",
                &args(json!({ "when": "2026-11-20T17:00:00Z" })),
            )
            .expect("setting the reminder it has is done, not refused");
        assert!(matches!(started, Started::Done(_)), "{started:?}");
        service.settle();
    }
}

const MAIL: (&str, &str) = (
    include_str!("../../../../apps/tl/mail/app.tl"),
    include_str!("../../../../apps/tl/mail/app.ids"),
);
const MAIL_APP: &str = "so.tilo.mail";

/// Ada's workspace with Mail's inbox a week in, as the session keeps it: a `ResourceService`
/// over the document, and its first conversation's published id.
fn mail_inbox() -> (ResourceService, String) {
    let (ada, loaded) = Replica::new("ada", Workspace::new(), &[MAIL]);
    let meta = loaded.last().unwrap().meta.clone();
    ada.host.root_of(MAIL_APP).unwrap();
    ada.commit();
    let scenario = meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "a busy inbox")
        .expect("Mail stages an inbox")
        .action
        .clone();
    let made = ada
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (scenario.as_str(), vec![]),
        )
        .unwrap();
    ada.commit();
    let tilolang_runtime::InvokeOutcome::Done(Value::Ref(conversation)) = made else {
        panic!("Mail's inbox stages a conversation");
    };
    let mut service = ada.into_service("ada");
    service.settle();
    (
        service,
        resource_id(MAIL_APP, &conversation.target.id_string()),
    )
}

/// An action that answers with a resource it made answers that resource, even where the rest of
/// its step waits on the person: `mail.answer(t, body)` writes the draft and then sends it, an
/// outside effect the workspace asks about, so its answer is the draft the program goes on with.
/// It answered nothing instead, and a program that named the draft it returned trapped with "the
/// action ran but did not answer the Draft it returns".
#[test]
fn an_answers_draft_is_the_answer_of_the_action_that_made_it() {
    let (mut service, conversation) = mail_inbox();
    let invoker = tilo_actions::ActionInvoker::agent("task-1").acting_for("ada");
    let started = service
        .invoke_as(
            &invoker,
            &conversation,
            "answer",
            &args(json!({ "body": "Friday works for me." })),
        )
        .expect("answering writes the draft and asks about the send");
    let Started::Done(output) = started else {
        panic!("answering is done once its draft is written: {started:?}");
    };
    let made = output
        .resources
        .first()
        .unwrap_or_else(|| panic!("the draft it made is its answer: {output:?}"));
    assert!(
        made.starts_with(&resource_id(MAIL_APP, "")),
        "the draft it made is published under Mail, whose draft is what `answer` returns: {made}"
    );
}
