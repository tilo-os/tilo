//! Undo in a room two people and an agent work in, with the shipped Notes and Tasks apps: each
//! person's undo takes back their own step and leaves everybody else's edits standing, and it
//! is never refused because somebody else changed the same record since.

use automerge::AutoCommit;
use collaboration::history_source::verbs::{EDIT_TYPE, HistoryEdit};
use serde_json::Value as Json;
use tilo_actions::{ActionContext, ActionInvoker, EncodedAction};
use tilo_resource::{Edit, EditOp, Id, Segment};
use tilolang_runtime::{ResourceRef, Value};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::instance::room::HostHistoryEdit;

use crate::fixtures::{Replica, field_id};
use crate::legibility::call;

const NOTES: &str = include_str!("../../../../apps/tl/notes/app.tl");
const NOTES_IDS: &str = include_str!("../../../../apps/tl/notes/app.ids");
const TASKS: &str = include_str!("../../../../apps/tl/tasks/app.tl");
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const NOTES_APP: &str = "so.tilo.lang.notes";
const TASKS_APP: &str = "so.tilo.lang.tasks";
const AGENT: &str = "agent:helper";
const RUN: &str = "issue-9@8000000000000001";

fn packages() -> [(&'static str, &'static str); 2] {
    [(NOTES, NOTES_IDS), (TASKS, TASKS_IDS)]
}

/// Ada's room with the two apps, and Ben's replica of it.
fn room() -> (Replica, Replica) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &packages());
    ada.host
        .runtime()
        .set_clock(|| "2026-10-30T09:00:00Z".to_owned());
    ada.commit();
    let mut ben = ada.fork("ben", &packages());
    ben.host.sync_from(&ben.doc);
    (ada, ben)
}

fn target(v: &Value) -> ResourceRef {
    match v {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

fn root(r: &Replica, app: &str) -> Value {
    Value::resource(&r.host.root_of(app).unwrap())
}

fn text(r: &Replica, item: &ResourceRef, field: &str) -> String {
    match r.host.runtime().read(item, field).unwrap() {
        Value::Text(text) => text,
        other => panic!("{field} is text: {other:?}"),
    }
}

/// `r`'s person edits `field` of `item` (of the type `ty` in `app`) with `op`, as typing in its
/// field does: one step of theirs.
fn edit(r: &mut Replica, app: &str, (item, ty, field): (&ResourceRef, &str, &str), op: EditOp) {
    let meta = r.host.package(app).unwrap().meta;
    let path = vec![
        Segment::Item(Id::from_raw(item.item.as_deref().unwrap())),
        Segment::Field(field_id(&meta, ty, field)),
    ];
    r.apply(&DynAction {
        app: app.to_owned(),
        kind: format!("tl.{app}.{ty}"),
        action_id: "$edit".to_owned(),
        args: Json::Null,
        authorship: Authorship::default(),
        edits: vec![DynEdit::Edit {
            id: item.doc.clone(),
            edit: Edit::new(path, op),
        }],
    });
}

fn type_at(r: &mut Replica, note: &ResourceRef, after: &str, typed: &str) {
    let body = text(r, note, "body");
    let at = body.find(after).unwrap() + after.chars().count();
    edit(
        r,
        NOTES_APP,
        (note, "Note", "body"),
        EditOp::Splice {
            at,
            delete: 0,
            insert: typed.to_owned(),
        },
    );
}

/// `r`'s person runs the history verb `edit` on the record, as their window's Undo does.
fn history(r: &mut Replica, edit: &HistoryEdit) {
    let payload = serde_json::to_vec(edit).unwrap();
    let action: HostHistoryEdit = serde_json::from_slice(&payload).unwrap();
    let context = ActionContext::for_resource("room", "history", "history")
        .with_invoker(ActionInvoker::actor(r.name.clone()));
    tilo_actions::apply_local_with_context(
        &mut r.doc,
        &action,
        EncodedAction {
            type_id: EDIT_TYPE.to_owned(),
            payload,
        },
        Some(r.name.as_bytes()),
        context,
    )
    .expect("undo is never refused");
    r.doc.commit();
    r.host.sync_from(&r.doc);
}

fn undo(r: &mut Replica) {
    history(
        r,
        &HistoryEdit::Undo {
            steps: None,
            run: None,
            by: None,
            to: None,
        },
    );
}

/// A note both people have, with `body`.
fn shared_note(ada: &mut Replica, ben: &mut Replica, body: &str) -> ResourceRef {
    let note = target(&call(
        ada,
        "ada",
        NOTES_APP,
        "new_note",
        vec![root(ada, NOTES_APP)],
    ));
    ada.commit();
    edit(
        ada,
        NOTES_APP,
        (&note, "Note", "body"),
        EditOp::Replace {
            text: body.to_owned(),
        },
    );
    ada.sync_with(ben);
    note
}

#[test]
fn two_people_typing_in_one_note_each_undo_only_their_own_text() {
    let (mut ada, mut ben) = room();
    let note = shared_note(&mut ada, &mut ben, "Shopping.\nChores.");
    type_at(&mut ada, &note, "Shopping.", " Milk and eggs.");
    type_at(&mut ben, &note, "Chores.", " Bins out.");
    ada.sync_with(&mut ben);
    assert_eq!(
        text(&ada, &note, "body"),
        "Shopping. Milk and eggs.\nChores. Bins out."
    );

    undo(&mut ada);
    ada.sync_with(&mut ben);
    assert_eq!(text(&ben, &note, "body"), "Shopping.\nChores. Bins out.");
    undo(&mut ben);
    ada.sync_with(&mut ben);
    assert_eq!(text(&ada, &note, "body"), "Shopping.\nChores.");
}

#[test]
fn two_people_typing_in_one_paragraph_each_undo_only_their_own_words() {
    let (mut ada, mut ben) = room();
    let note = shared_note(&mut ada, &mut ben, "The fox jumps.");
    // Concurrently, in the same paragraph: neither has seen the other's words.
    type_at(&mut ada, &note, "The", " quick");
    type_at(&mut ben, &note, "jumps", " high");
    type_at(&mut ben, &note, "The", " brown");
    ada.sync_with(&mut ben);
    let both = text(&ada, &note, "body");
    assert!(both.contains("quick") && both.contains("brown") && both.ends_with("jumps high."));

    undo(&mut ada);
    ada.sync_with(&mut ben);
    let body = text(&ada, &note, "body");
    assert!(!body.contains("quick"), "{body}");
    assert_eq!(body, "The brown fox jumps high.");
    undo(&mut ben);
    undo(&mut ben);
    ada.sync_with(&mut ben);
    assert_eq!(text(&ada, &note, "body"), "The fox jumps.");
}

#[test]
fn a_task_retitled_by_somebody_else_since_keeps_their_title_when_its_first_title_is_undone() {
    let (mut ada, mut ben) = room();
    let task = target(&call(
        &ada,
        "ada",
        TASKS_APP,
        "add",
        vec![root(&ada, TASKS_APP), Value::text("")],
    ));
    ada.commit();
    let title = |text: &str| EditOp::Replace {
        text: text.to_owned(),
    };
    edit(&mut ada, TASKS_APP, (&task, "Task", "title"), title("Milk"));
    call(
        &ada,
        "ada",
        TASKS_APP,
        "check",
        vec![Value::resource(&task), Value::Bool(true)],
    );
    ada.commit();
    ada.sync_with(&mut ben);
    edit(
        &mut ben,
        TASKS_APP,
        (&task, "Task", "title"),
        title("Oat milk"),
    );
    call(
        &ben,
        "ben",
        TASKS_APP,
        "check",
        vec![Value::resource(&task), Value::Bool(false)],
    );
    ben.commit();
    ada.sync_with(&mut ben);

    // Ada takes back her check, then her title: Ben set both since, so his stay.
    undo(&mut ada);
    undo(&mut ada);
    ada.sync_with(&mut ben);
    assert_eq!(text(&ada, &task, "title"), "Oat milk");
    assert_eq!(
        ada.host.runtime().read(&task, "done").unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn a_removed_task_comes_back_after_the_task_beside_it_changed() {
    let (mut ada, mut ben) = room();
    let tasks = root(&ada, TASKS_APP);
    let milk = target(&call(
        &ada,
        "ada",
        TASKS_APP,
        "add",
        vec![tasks.clone(), Value::text("Milk")],
    ));
    let eggs = target(&call(
        &ada,
        "ada",
        TASKS_APP,
        "add",
        vec![tasks.clone(), Value::text("Eggs")],
    ));
    ada.commit();
    ada.sync_with(&mut ben);
    call(
        &ada,
        "ada",
        TASKS_APP,
        "remove",
        vec![tasks.clone(), Value::resource(&milk)],
    );
    ada.commit();
    edit(
        &mut ben,
        TASKS_APP,
        (&eggs, "Task", "title"),
        EditOp::Replace {
            text: "Free-range eggs".to_owned(),
        },
    );
    call(
        &ben,
        "ben",
        TASKS_APP,
        "check",
        vec![Value::resource(&eggs), Value::Bool(true)],
    );
    ben.commit();
    ada.sync_with(&mut ben);

    undo(&mut ada);
    ada.sync_with(&mut ben);
    for r in [&ada, &ben] {
        assert_eq!(text(r, &milk, "title"), "Milk", "{}", r.name);
        assert_eq!(text(r, &eggs, "title"), "Free-range eggs", "{}", r.name);
        assert_eq!(
            r.host.runtime().read(&eggs, "done").unwrap(),
            Value::Bool(true)
        );
    }
}

#[test]
fn an_agent_run_undone_while_a_person_edited_the_same_note_keeps_the_persons_edits() {
    let (mut ada, mut ben) = room();
    let note = shared_note(&mut ada, &mut ben, "Plan the trip.");
    call(
        &ada,
        AGENT,
        NOTES_APP,
        "retitle",
        vec![Value::resource(&note), Value::text("Trip")],
    );
    call(
        &ada,
        AGENT,
        NOTES_APP,
        "pin",
        vec![Value::resource(&note), Value::Bool(true)],
    );
    ada.commit_in_run(AGENT, RUN, "Tidy my notes");
    ada.sync_with(&mut ben);
    // Ben edits the same note meanwhile: its body, and its title the agent set.
    type_at(&mut ben, &note, "Plan", " and enjoy");
    edit(
        &mut ben,
        NOTES_APP,
        (&note, "Note", "title"),
        EditOp::Replace {
            text: "Trip to Rome".to_owned(),
        },
    );
    ada.sync_with(&mut ben);

    history(
        &mut ada,
        &HistoryEdit::Undo {
            steps: None,
            run: Some(RUN.to_owned()),
            by: None,
            to: None,
        },
    );
    ada.sync_with(&mut ben);
    for r in [&ada, &ben] {
        assert_eq!(
            text(r, &note, "body"),
            "Plan and enjoy the trip.",
            "{}",
            r.name
        );
        assert_eq!(text(r, &note, "title"), "Trip to Rome", "{}", r.name);
        assert_eq!(
            r.host.runtime().read(&note, "pinned").unwrap(),
            Value::Bool(false),
            "the run's pin is taken back on {}",
            r.name
        );
    }
}
