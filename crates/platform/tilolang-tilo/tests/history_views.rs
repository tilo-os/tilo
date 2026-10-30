//! History in views (`std.history`): each step says who made it and what it did; a change shows
//! what each field held before and after as the field's type shows it (a title as text, a due
//! day as a day), read from the change itself; consecutive changes by one person or one task run
//! fold into one entry; and Undo on an entry takes back exactly that step, or the whole run,
//! through the record's history verb.

use automerge::AutoCommit;
use tilolang_runtime::{ResourceRef, UiNode, UndoAsk, Value, ViewEvent, ViewInstance};
use tilolang_tilo::instance::HistoryAsk;

use crate::fixtures::Replica;
use crate::legibility::{AGENT, NOTES_APP, RUN, Tidied, a_run_in_notes_and_tasks, call};

const NOTES: &str = include_str!("../../../../apps/tl/notes/app.tl");
const NOTES_IDS: &str = include_str!("../../../../apps/tl/notes/app.ids");
const TASKS: &str = include_str!("../../../../apps/tl/tasks/app.tl");
const TASKS_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const TASKS_APP: &str = "so.tilo.lang.tasks";

fn walk<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
    out.push(node);
    for child in &node.children {
        walk(child, out);
    }
}

fn nodes(node: &UiNode) -> Vec<&UiNode> {
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

fn text(node: &UiNode) -> Option<&str> {
    match node.props.get("value").or_else(|| node.props.get("label")) {
        Some(Value::Text(text)) => Some(text),
        _ => None,
    }
}

/// The history section of a window: the `section` titled as history is.
fn history(view: &mut ViewInstance) -> UiNode {
    view.poll_patches().unwrap();
    nodes(view.tree())
        .into_iter()
        .find(|node| {
            node.kind == "ui.Section"
                && node.props.get("title") == Some(&Value::text("tilolang-history"))
        })
        .cloned()
        .unwrap_or_else(|| panic!("the window shows its history: {:#?}", view.tree()))
}

/// Each entry of a history: the avatar that heads it, by who it shows.
fn entries(section: &UiNode) -> Vec<String> {
    nodes(section)
        .into_iter()
        .filter(|node| node.kind == "ui.Avatar" && !node.key.contains('~'))
        .filter_map(|node| match node.props.get("who") {
            Some(Value::Principal(who)) => Some(who.0.clone()),
            _ => None,
        })
        .collect()
}

/// Each change a history shows: the field's name, then what stands for what it held before
/// and after (the node showing each).
fn changes(section: &UiNode) -> Vec<(String, UiNode, UiNode)> {
    let mut out = Vec::new();
    for grid in nodes(section).into_iter().filter(|n| n.kind == "grid") {
        for cells in grid.children.chunks(3) {
            if let [label, before, after] = cells {
                out.push((
                    text(label).unwrap_or_default().to_owned(),
                    before.clone(),
                    after.clone(),
                ));
            }
        }
    }
    out
}

/// The key of the button saying `label` within `node`.
fn button(node: &UiNode, label: &str) -> String {
    nodes(node)
        .into_iter()
        .find(|n| n.kind == "ui.Button" && text(n) == Some(label))
        .map(|n| n.key.clone())
        .unwrap_or_else(|| panic!("a `{label}` button: {node:#?}"))
}

fn target(v: &Value) -> ResourceRef {
    match v {
        Value::Ref(r) => (*r.target).clone(),
        other => panic!("a resource: {other:?}"),
    }
}

fn field(r: &Replica, app: &str, at: &ResourceRef, ty: &str, name: &str) -> Value {
    let id = crate::fixtures::field_id(&r.host.package(app).unwrap().meta, ty, name);
    r.host.runtime().read(at, &id).unwrap()
}

/// Press `key` in `view` and carry out the undo it asks, as the window's host does.
fn press_undo(r: &mut Replica, view: &mut ViewInstance, key: &str) -> UndoAsk {
    view.press(key).unwrap();
    let ask = view
        .poll_events()
        .into_iter()
        .find_map(|event| match event {
            ViewEvent::Undo(ask) => Some(ask),
            _ => None,
        })
        .expect("pressing Undo asks the history to undo");
    let asked = match &ask {
        UndoAsk::Step { change, .. } => HistoryAsk::UndoStep(change.clone()),
        UndoAsk::Run { run } => HistoryAsk::UndoRun(run.clone()),
    };
    r.history(&asked.edit());
    ask
}

fn two_people() -> (Replica, Value, Value) {
    let (mut ada, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[(NOTES, NOTES_IDS), (TASKS, TASKS_IDS)],
    );
    ada.host
        .runtime()
        .set_clock(|| "2026-10-30T09:00:00Z".to_owned());
    let notes = Value::resource(&ada.host.root_of(NOTES_APP).unwrap());
    let tasks = Value::resource(&ada.host.root_of(TASKS_APP).unwrap());
    ada.commit();
    (ada, notes, tasks)
}

#[test]
fn a_change_shows_what_the_title_and_the_due_day_held_before_and_after() {
    let (mut ada, notes, tasks) = two_people();
    let note = call(&ada, "ada", NOTES_APP, "new_note", vec![notes]);
    call(
        &ada,
        "ada",
        NOTES_APP,
        "retitle",
        vec![note.clone(), Value::text("Groceries")],
    );
    ada.commit();
    // Bo renames it: a step of his own, after Ada's.
    call(
        &ada,
        "bo",
        NOTES_APP,
        "retitle",
        vec![note.clone(), Value::text("Shopping")],
    );
    ada.commit();
    let task = call(
        &ada,
        "ada",
        TASKS_APP,
        "add",
        vec![tasks, Value::text("Call the landlord")],
    );
    let day = |d: &str| Value::some(Value::text(d));
    call(
        &ada,
        "ada",
        TASKS_APP,
        "schedule",
        vec![task.clone(), day("2026-11-02")],
    );
    ada.commit();
    call(
        &ada,
        "bo",
        TASKS_APP,
        "schedule",
        vec![task.clone(), day("2026-11-05")],
    );
    ada.commit();

    // The note's window: Bo's rename, newest, says what the title held and holds.
    let mut editor = ada.host.open_resource(&target(&note), None).unwrap();
    let section = history(&mut editor);
    assert_eq!(
        entries(&section),
        ["bo", "ada"],
        "newest first, one entry each"
    );
    let shown = changes(&section);
    let (label, before, after) = shown.first().expect("the rename's change is shown");
    assert_eq!(label, "Title");
    assert_eq!(before.kind, "ui.Text");
    assert_eq!(text(before), Some("Groceries"));
    assert_eq!(before.props.get("secondary"), Some(&Value::Bool(true)));
    assert_eq!(text(after), Some("Shopping"));
    // What the action that made it says it does, from its declaration.
    assert!(
        nodes(&section)
            .iter()
            .any(|n| text(n) == Some("Renames a note.")),
        "the entry says what was done: {section:#?}"
    );

    // The task's window: the due day before and after, each shown as a day.
    let mut page = ada.host.open_resource(&target(&task), None).unwrap();
    let section = history(&mut page);
    let (label, before, after) = changes(&section)
        .into_iter()
        .next()
        .expect("the new due day is shown");
    assert_eq!(label, "Due");
    for (node, day) in [(&before, "2026-11-02"), (&after, "2026-11-05")] {
        assert_eq!(node.props.get("value"), Some(&Value::text(day)), "{node:?}");
        assert_eq!(
            node.props.get("value:as"),
            Some(&Value::text("date")),
            "{node:?}"
        );
    }
}

#[test]
fn undo_on_an_older_step_takes_back_that_step_alone() {
    let (mut ada, notes, _) = two_people();
    let note = call(&ada, "ada", NOTES_APP, "new_note", vec![notes]);
    ada.commit();
    call(
        &ada,
        "ada",
        NOTES_APP,
        "retitle",
        vec![note.clone(), Value::text("Groceries")],
    );
    ada.commit();
    call(
        &ada,
        "ada",
        NOTES_APP,
        "tag",
        vec![note.clone(), Value::text("food")],
    );
    ada.commit();
    let at = target(&note);
    let mut editor = ada.host.open_resource(&at, None).unwrap();
    // Ada's three changes are one entry; it opens to each of them.
    let section = history(&mut editor);
    assert_eq!(entries(&section), ["ada"]);
    editor
        .press(&button(&section, "tilolang-history-show"))
        .unwrap();
    let section = history(&mut editor);
    // The rename, older than the tag: its own Undo.
    let rename = nodes(&section)
        .into_iter()
        .find(|n| {
            n.kind == "stack"
                && n.children.first().is_some_and(|row| {
                    row.children
                        .iter()
                        .any(|t| text(t) == Some("Renames a note."))
                })
        })
        .cloned()
        .expect("the rename has a row of its own");
    let asked = press_undo(&mut ada, &mut editor, &button(&rename, "tilolang-undo"));
    assert!(matches!(asked, UndoAsk::Step { target, .. } if target == at));
    assert_eq!(
        field(&ada, NOTES_APP, &at, "Note", "title"),
        Value::text("")
    );
    assert_eq!(
        field(&ada, NOTES_APP, &at, "Note", "tags"),
        Value::List(vec![Value::text("food")]),
        "the later tag stays"
    );
}

#[test]
fn undo_run_from_the_history_takes_back_every_step_of_the_run() {
    let Tidied {
        mut ada,
        groceries,
        landlord,
        ..
    } = a_run_in_notes_and_tasks();
    let mut editor = ada.host.open_resource(&groceries, None).unwrap();
    let section = history(&mut editor);
    // The run's rename, then Ada's own making and naming of the note, each one entry.
    assert_eq!(entries(&section), [AGENT, "ada"]);
    assert!(
        nodes(&section)
            .iter()
            .any(|n| text(n) == Some("Tidy my notes and tasks")),
        "the run's entry says what it was asked"
    );
    let asked = press_undo(
        &mut ada,
        &mut editor,
        &button(&section, "tilolang-history-undo-run"),
    );
    assert_eq!(
        asked,
        UndoAsk::Run {
            run: RUN.to_owned()
        }
    );
    assert_eq!(
        field(&ada, NOTES_APP, &groceries, "Note", "title"),
        Value::text("Groceries")
    );
    assert_eq!(
        field(&ada, TASKS_APP, &landlord, "Task", "done"),
        Value::Bool(false),
        "the run's change in Tasks is taken back with it"
    );
}

#[test]
fn consecutive_changes_by_one_person_or_one_run_fold_into_one_entry() {
    let (mut ada, notes, _) = two_people();
    let note = call(&ada, "ada", NOTES_APP, "new_note", vec![notes]);
    call(
        &ada,
        "ada",
        NOTES_APP,
        "retitle",
        vec![note.clone(), Value::text("Plan")],
    );
    ada.commit();
    for title in ["Plan A", "Plan B", "Plan C"] {
        call(
            &ada,
            AGENT,
            NOTES_APP,
            "retitle",
            vec![note.clone(), Value::text(title)],
        );
    }
    ada.commit_in_run(AGENT, RUN, "Tidy the notes");
    call(
        &ada,
        "ada",
        NOTES_APP,
        "pin",
        vec![note.clone(), Value::Bool(true)],
    );
    ada.commit();
    let mut editor = ada.host.open_resource(&target(&note), None).unwrap();
    let section = history(&mut editor);
    // Ada's pin, the run's three renames, Ada's making and naming: three entries.
    assert_eq!(entries(&section), ["ada", AGENT, "ada"]);
    let shows = button(&section, "tilolang-history-show");
    // The run's entry: who, what it was asked, how many changes; opened, each of them.
    assert!(
        nodes(&section)
            .iter()
            .any(|n| text(n) == Some("Tidy the notes"))
    );
    editor.press(&shows).unwrap();
    let section = history(&mut editor);
    let renames: Vec<String> = changes(&section)
        .into_iter()
        .filter(|(label, ..)| label == "Title")
        .filter_map(|(_, _, after)| text(&after).map(str::to_owned))
        .collect();
    assert_eq!(
        renames,
        ["Plan A", "Plan B", "Plan C"],
        "the run's entry opens to each rename"
    );
}
