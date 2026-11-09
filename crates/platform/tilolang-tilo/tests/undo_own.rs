//! Undo in a workspace two people and an agent work in, with the shipped Notes and Tasks apps: each
//! person's undo takes back their own step and leaves everybody else's edits standing, and it
//! is never refused because somebody else changed the same record since.

use automerge::AutoCommit;
use collaboration::history_source::verbs::{EDIT_TYPE, HistoryEdit};
use serde_json::Value as Json;
use tilo_actions::{ActionContext, ActionInvoker, EncodedAction};
use tilo_resource::{Edit, EditOp, Id, Segment};
use tilolang_runtime::{ResourceRef, Value};
use tilolang_tilo::action::{Authorship, DynAction, DynEdit};
use tilolang_tilo::instance::record::HostHistoryEdit;

use crate::fixtures::{Replica, field_id};
use crate::legibility::call;

const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
const NOTES: &str = include_str!("../../../../apps/tl/notes/app.tl");
const NOTES_IDS: &str = include_str!("../../../../apps/tl/notes/app.ids");
const TASKS: &str = crate::fixtures::TASKS_SOURCE;
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const NOTES_APP: &str = "so.tilo.lang.notes";
const TASKS_APP: &str = "so.tilo.lang.tasks";
const AGENT: &str = "agent:helper";
const RUN: &str = "issue-9@8000000000000001";

fn packages() -> [(&'static str, &'static str); 3] {
    [
        (DISCUSSION, DISCUSSION_IDS),
        (NOTES, NOTES_IDS),
        (TASKS, TASKS_IDS),
    ]
}

/// Ada's workspace with the two apps, and Ben's replica of it.
fn workspace() -> (Replica, Replica) {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &packages());
    ada.host.set_clock(|| "2026-10-30T09:00:00Z".to_owned());
    ada.commit();
    let ben = ada.fork("ben", &packages());
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
        Value::Rich(rich) => rich.written_text(),
        other => panic!("{field} is text: {other:?}"),
    }
}

/// The rich value `r` holds of `field` of `item`.
fn rich(r: &Replica, item: &ResourceRef, field: &str) -> tilolang_runtime::rich::RichValue {
    match r.host.runtime().read(item, field).unwrap() {
        Value::Rich(rich) => rich,
        other => panic!("{field} is rich text: {other:?}"),
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

/// `r`'s person types `typed` right after the first `after` in the note's rich body, as the
/// shared rich input does: a rich splice in that paragraph.
fn type_at(r: &mut Replica, note: &ResourceRef, after: &str, typed: &str) {
    let body = rich(r, note, "body");
    let (block, at) = body
        .blocks
        .iter()
        .find_map(|block| {
            block
                .text
                .find(after)
                .map(|from| (block.id.clone(), from.saturating_add(after.chars().count())))
        })
        .unwrap_or_else(|| panic!("the body says {after}: {:?}", body.written_text()));
    let action = tilo_document::RichTextAction::SpliceText {
        region: tilo_document::action::RegionKind::Body,
        block,
        at,
        del: 0,
        ins: typed.to_owned(),
    };
    let rich = serde_json::to_value(&action).unwrap();
    edit(r, NOTES_APP, (note, "Note", "body"), EditOp::Rich { rich });
}

/// `r`'s person runs the history verb `edit` on the record, as their window's Undo does.
fn history(r: &mut Replica, edit: &HistoryEdit) {
    let payload = serde_json::to_vec(edit).unwrap();
    let action: HostHistoryEdit = serde_json::from_slice(&payload).unwrap();
    let context = ActionContext::for_resource("workspace", "history", "history")
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
            step: None,
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
    // The body as a paragraph per line, no marks: the same words a plain text held.
    let blocks: Vec<tilo_document::ClipBlock> = tilo_document::blocks_of_plain(body)
        .iter()
        .map(tilo_document::ClipBlock::from_view)
        .collect();
    let action = tilo_document::RichTextAction::SyncClips { blocks };
    let rich = serde_json::to_value(&action).unwrap();
    edit(
        ada,
        NOTES_APP,
        (&note, "Note", "body"),
        EditOp::Rich { rich },
    );
    ada.sync_with(ben);
    note
}

#[test]
fn two_people_typing_in_one_note_each_undo_only_their_own_text() {
    let (mut ada, mut ben) = workspace();
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
    let (mut ada, mut ben) = workspace();
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
    let (mut ada, mut ben) = workspace();
    let task = target(&call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&root(&ada, TASKS_APP), ""),
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
    let (mut ada, mut ben) = workspace();
    let tasks = root(&ada, TASKS_APP);
    let milk = target(&call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Milk"),
    ));
    let eggs = target(&call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Eggs"),
    ));
    ada.commit();
    ada.sync_with(&mut ben);
    call(
        &ada,
        "ada",
        TASKS_APP,
        "remove",
        vec![tasks, Value::resource(&milk)],
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
    let (mut ada, mut ben) = workspace();
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
            step: None,
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

/// The tasks of the workspace's list, in the order they stand.
fn tasks_in(r: &Replica) -> Vec<Value> {
    match r
        .host
        .runtime()
        .read(&target(&root(r, TASKS_APP)), "items")
        .unwrap()
    {
        Value::List(items) => items,
        other => panic!("a list: {other:?}"),
    }
}

/// The task with this title, as its person's list holds it.
fn task_titled(r: &Replica, title: &str) -> ResourceRef {
    tasks_in(r)
        .into_iter()
        .map(|item| target(&item))
        .find(|task| text(r, task, "title") == title)
        .unwrap_or_else(|| panic!("{title}"))
}

/// Whether a task is done.
fn done(r: &Replica, task: &ResourceRef) -> Value {
    r.host.runtime().read(task, "done").unwrap()
}

/// A check is one step: one person's Undo opens the task and every task it took with it, and
/// the steps before it stand.
#[test]
fn undo_opens_a_checked_task_and_every_task_under_it_at_once() {
    let (mut ada, _ben) = workspace();
    let tasks = root(&ada, TASKS_APP);
    let parent = target(&call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Write the launch post"),
    ));
    ada.commit();
    call(
        &ada,
        "ada",
        TASKS_APP,
        "add_subtask",
        vec![
            tasks.clone(),
            Value::resource(&parent),
            Value::text("Outline"),
        ],
    );
    ada.commit();
    let outline = task_titled(&ada, "Outline");
    call(
        &ada,
        "ada",
        TASKS_APP,
        "add_subtask",
        vec![
            tasks,
            Value::resource(&outline),
            Value::text("Gather facts"),
        ],
    );
    ada.commit();
    let facts = task_titled(&ada, "Gather facts");
    call(
        &ada,
        "ada",
        TASKS_APP,
        "check",
        vec![Value::resource(&parent)],
    );
    ada.commit();
    for task in [&parent, &outline, &facts] {
        assert_eq!(done(&ada, task), Value::Bool(true));
    }

    undo(&mut ada);
    for task in [&parent, &outline, &facts] {
        assert_eq!(
            done(&ada, task),
            Value::Bool(false),
            "one undo opens the whole check"
        );
    }
    assert_eq!(tasks_in(&ada).len(), 3, "the tasks are still there");
    // The adds before it are steps of their own: the next Undo takes the last of them back.
    undo(&mut ada);
    assert_eq!(
        tasks_in(&ada).len(),
        2,
        "the check was one step, so the undo before it was the add"
    );
}

/// The tasks as they stand, by id.
fn order(r: &Replica, tasks: &ResourceRef) -> Vec<String> {
    match r.host.runtime().read(tasks, "items").unwrap() {
        Value::List(items) => items.iter().map(|item| target(item).id_string()).collect(),
        other => panic!("a list of tasks: {other:?}"),
    }
}

/// The task a task stands under, by id.
fn under(r: &Replica, task: &ResourceRef) -> Option<String> {
    match r.host.runtime().read(task, "parent").unwrap() {
        Value::Opt(Some(parent)) => Some(target(&parent).id_string()),
        Value::Opt(None) => None,
        other => panic!("a parent: {other:?}"),
    }
}

/// Indenting a task under the one over it, and outdenting it again, are one step each: undo
/// takes the task back out from under its parent, and the next undo puts it back there.
#[test]
fn a_task_indented_under_another_is_put_back_in_one_undo() {
    let (mut ada, _) = workspace();
    let tasks = target(&root(&ada, TASKS_APP));
    let make = |ada: &Replica, title: &str| {
        target(&call(
            ada,
            "ada",
            TASKS_APP,
            "new_task",
            crate::tasks::create(&Value::resource(&tasks), title),
        ))
    };
    let book = make(&ada, "Book a haircut");
    let find = make(&ada, "Find a salon");
    let ring = make(&ada, "Call the salon");
    ada.commit();
    let id = |task: &ResourceRef| task.id_string();

    // Tab puts each task under the task the list shows over it at its level, the first task
    // here for both: one change each.
    for (task, above) in [(&find, &book), (&ring, &book)] {
        call(
            &ada,
            "ada",
            TASKS_APP,
            "indent",
            vec![Value::resource(task), Value::some(Value::resource(above))],
        );
        ada.commit();
    }
    assert_eq!(under(&ada, &ring).as_deref(), Some(id(&book).as_str()));
    assert_eq!(
        order(&ada, &tasks),
        [id(&book), id(&find), id(&ring)],
        "the subtasks stand under their parent, in order"
    );

    // Shift+Tab lifts the second out beside its parent, and one undo puts it back.
    call(
        &ada,
        "ada",
        TASKS_APP,
        "outdent",
        vec![Value::resource(&ring)],
    );
    ada.commit();
    assert_eq!(under(&ada, &ring), None);
    assert_eq!(
        order(&ada, &tasks),
        [id(&book), id(&ring), id(&find)],
        "out again it stands beside the task it was under"
    );
    undo(&mut ada);
    assert_eq!(
        under(&ada, &ring).as_deref(),
        Some(id(&book).as_str()),
        "one undo takes the outdent back"
    );
    assert_eq!(
        order(&ada, &tasks),
        [id(&book), id(&find), id(&ring)],
        "and stands it under its parent again"
    );

    // The undo of the indent itself takes the task back out to the top level.
    undo(&mut ada);
    assert_eq!(under(&ada, &ring), None);
    assert_eq!(order(&ada, &tasks), [id(&book), id(&find), id(&ring)]);
}
