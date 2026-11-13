//! History in views (`std.history`): each step says who made it and what it did; a change shows
//! what each field held before and after as the field's type shows it (a title as text, a due
//! day as a day), read from the change itself; consecutive changes by one person or one run
//! fold into one entry; and Undo on an entry takes back exactly that step, or the whole run,
//! through the record's history verb.

use automerge::AutoCommit;
use tilolang_runtime::{ResourceRef, Store, UiNode, UndoAsk, Value, ViewEvent, ViewInstance};
use tilolang_tilo::instance::HistoryAsk;

use crate::fixtures::Replica;
use crate::legibility::{AGENT, NOTES_APP, RUN, Tidied, a_run_in_notes_and_tasks, call};

const DISCUSSION: &str = include_str!("../../../../apps/tl/discussion/app.tl");
const DISCUSSION_IDS: &str = include_str!("../../../../apps/tl/discussion/app.ids");
const NOTES: &str = include_str!("../../../../apps/tl/notes/app.tl");
const NOTES_IDS: &str = include_str!("../../../../apps/tl/notes/app.ids");
const TASKS: &str = crate::fixtures::TASKS_SOURCE;
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

/// The narrowest of `found`: the innermost entry of history that matches, never the step that
/// holds it, whose row holds its changes' rows too.
fn narrowest(found: Vec<&UiNode>) -> Option<&UiNode> {
    found.into_iter().min_by_key(|node| nodes(node).len())
}

/// Each entry of a history: the avatar that heads it, by who it shows.
fn entries(section: &UiNode) -> Vec<String> {
    nodes(section)
        .into_iter()
        .filter(|node| node.kind == "ui.Avatar")
        .filter_map(|node| match node.props.get("who") {
            Some(Value::Principal(who)) => Some(who.0.clone()),
            _ => None,
        })
        .collect()
}

/// Each change a history shows: the field's name, then what stands for what it held before
/// and after (the node showing each).
fn changes(section: &UiNode) -> Vec<(String, UiNode, UiNode)> {
    /// A grid's cells: its children, with what a `for` places (a fragment) taken as its own.
    fn cells<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
        for child in &node.children {
            if child.kind == "fragment" {
                cells(child, out);
            } else {
                out.push(child);
            }
        }
    }
    let mut out = Vec::new();
    for grid in nodes(section).into_iter().filter(|n| n.kind == "grid") {
        let mut all = Vec::new();
        cells(grid, &mut all);
        for cells in all.chunks(3) {
            if let [label, before, after] = cells {
                out.push((
                    text(label).unwrap_or_default().to_owned(),
                    (*before).clone(),
                    (*after).clone(),
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
        .map_or_else(
            || panic!("a `{label}` button: {node:#?}"),
            |n| n.key.clone(),
        )
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

/// Fire `event` of `key` in `view` and carry out the undo it asks, as the window's host does.
fn fire_undo(r: &mut Replica, view: &mut ViewInstance, key: &str, event: &str) -> UndoAsk {
    view.fire(key, event).unwrap();
    let ask = view
        .poll_events()
        .into_iter()
        .find_map(|event| match event {
            ViewEvent::Undo(ask) => Some(ask),
            _ => None,
        })
        .expect("the control asks the history to undo");
    let asked = match &ask {
        UndoAsk::Step { change, .. } => HistoryAsk::UndoStep(change.clone()),
        UndoAsk::Run { run } => HistoryAsk::UndoRun(run.clone()),
    };
    r.history(&asked.edit());
    ask
}

/// Press `key` in `view` and carry out the undo it asks, as the window's host does.
fn press_undo(r: &mut Replica, view: &mut ViewInstance, key: &str) -> UndoAsk {
    fire_undo(r, view, key, "press")
}

/// Choose `key` in `view` — a command of a menu — and carry out the undo it asks.
fn choose_undo(r: &mut Replica, view: &mut ViewInstance, key: &str) -> UndoAsk {
    fire_undo(r, view, key, "choose")
}

fn two_people() -> (Replica, Value, Value) {
    let (mut ada, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[
            (DISCUSSION, DISCUSSION_IDS),
            (NOTES, NOTES_IDS),
            (TASKS, TASKS_IDS),
        ],
    );
    ada.host.set_clock(|| "2026-10-30T09:00:00Z".to_owned());
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
        "new_task",
        crate::tasks::create(&tasks, "Call the landlord"),
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
    // What it did, in the words of the field it wrote: it renamed the note.
    assert!(
        reads_as(&section).contains("bo changed title"),
        "the entry says what was done: {section:#?}"
    );

    // The task's window: the due day, shown as a day in the sentence that moved it there, and
    // not repeated under it.
    let mut page = ada.host.open_resource(&target(&task), None).unwrap();
    let section = history(&mut page);
    let days: Vec<&UiNode> = nodes(&section)
        .into_iter()
        .filter(|node| node.props.get("value") == Some(&Value::text("2026-11-05")))
        .collect();
    assert_eq!(days.len(), 1, "the new day is shown once: {section:#?}");
    assert_eq!(
        days[0].props.get("value:as"),
        Some(&Value::text("date")),
        "{:?}",
        days[0]
    );
    assert!(
        !changes(&section).iter().any(|(label, _, _)| label == "Due"),
        "the sentence says the day, so nothing stands muted under it"
    );
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
    let rename = narrowest(
        nodes(&section)
            .into_iter()
            .filter(|n| n.kind == "row")
            .filter(|row| {
                let held = nodes(row);
                held.iter().any(|t| text(t) == Some("changed title"))
                    && held
                        .iter()
                        .any(|t| t.kind == "ui.Button" && text(t) == Some("tilolang-undo"))
            })
            .collect(),
    )
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

/// The row of the entry that says `said`, the innermost one holding it.
fn entry_row(section: &UiNode, said: &str) -> UiNode {
    narrowest(
        nodes(section)
            .into_iter()
            .filter(|node| node.kind == "row")
            .filter(|row| nodes(row).into_iter().any(|held| text(held) == Some(said)))
            .collect(),
    )
    .cloned()
    .unwrap_or_else(|| panic!("an entry saying {said}: {section:#?}"))
}

/// Whether the controls `node` holds offer anything to take a change back with.
fn takes_back(node: &UiNode) -> bool {
    nodes(node)
        .iter()
        .any(|n| matches!(n.kind.as_str(), "ui.Button" | "ui.MenuButton"))
}

/// What history offers a person: the entry that made the thing has nothing to take back — undoing
/// it would remove the thing whose page it stands on — and a change of theirs older than the
/// newest waits in that entry's menu, not in a button beside every row.
#[test]
fn a_history_entry_never_takes_back_the_making_and_keeps_older_changes_in_its_menu() {
    let (mut ada, notes, _) = two_people();
    let note = call(&ada, "ada", NOTES_APP, "new_note", vec![notes]);
    ada.commit();
    call(
        &ada,
        "bo",
        NOTES_APP,
        "retitle",
        vec![note.clone(), Value::text("Shopping")],
    );
    ada.commit();
    call(
        &ada,
        "ada",
        NOTES_APP,
        "pin",
        vec![note.clone(), Value::Bool(true)],
    );
    ada.commit();
    call(
        &ada,
        "bo",
        NOTES_APP,
        "tag",
        vec![note.clone(), Value::text("food")],
    );
    ada.commit();
    let at = target(&note);
    let mut editor = ada.host.open_resource(&at, None).unwrap();
    let section = history(&mut editor);
    // Bo's tag, Ada's pin, Bo's rename and Ada's making the note: newest first.
    assert_eq!(entries(&section), ["bo", "ada", "bo", "ada"]);

    let making = entry_row(&section, "created it");
    assert!(
        !takes_back(&making),
        "the making offers nothing to take back: {making:#?}"
    );
    let theirs = entry_row(&section, "changed title");
    assert!(
        !takes_back(&theirs),
        "a change of someone else's is not taken back from here: {theirs:#?}"
    );

    // Ada's pin, older than Bo's tag: its row offers one quiet menu, holding its revert.
    let older = entry_row(&section, "tilolang-history-actions");
    assert!(
        !nodes(&older).iter().any(|n| n.kind == "ui.Button"),
        "an older change is no button of its row: {older:#?}"
    );
    let revert = nodes(&older)
        .into_iter()
        .find(|n| n.kind == "ui.MenuItem")
        .expect("the menu holds the revert");
    assert_eq!(text(revert), Some("tilolang-history-revert"));

    let asked = choose_undo(&mut ada, &mut editor, &revert.key.clone());
    assert!(matches!(asked, UndoAsk::Step { target, .. } if target == at));
    assert_eq!(
        field(&ada, NOTES_APP, &at, "Note", "pinned"),
        Value::Bool(false),
        "the revert took the pin back"
    );
    assert_eq!(
        field(&ada, NOTES_APP, &at, "Note", "title"),
        Value::text("Shopping"),
        "Bo's later rename stays"
    );
}

/// The change a page ends with is the one a person means by undo: it offers it where it stands,
/// quietly, and taking it back asks the history to reverse that change.
#[test]
fn the_newest_history_entry_offers_the_persons_own_change_undone_on_the_row() {
    let (mut ada, notes, _) = two_people();
    let note = call(&ada, "ada", NOTES_APP, "new_note", vec![notes]);
    ada.commit();
    call(
        &ada,
        "bo",
        NOTES_APP,
        "retitle",
        vec![note.clone(), Value::text("Groceries")],
    );
    ada.commit();
    call(
        &ada,
        "ada",
        NOTES_APP,
        "pin",
        vec![note.clone(), Value::Bool(true)],
    );
    ada.commit();
    let at = target(&note);
    let mut editor = ada.host.open_resource(&at, None).unwrap();
    let section = history(&mut editor);
    assert_eq!(entries(&section), ["ada", "bo", "ada"]);
    let making = entry_row(&section, "created it");
    assert!(
        !takes_back(&making),
        "the making offers nothing to take back: {making:#?}"
    );
    let newest = entry_row(&section, "tilolang-undo");
    assert!(
        !nodes(&newest).iter().any(|n| n.kind == "ui.MenuButton"),
        "the newest change offers its undo, not a menu: {newest:#?}"
    );

    let asked = press_undo(&mut ada, &mut editor, &button(&newest, "tilolang-undo"));
    assert!(matches!(asked, UndoAsk::Step { target, .. } if target == at));
    assert_eq!(
        field(&ada, NOTES_APP, &at, "Note", "pinned"),
        Value::Bool(false),
        "undo took the pin back"
    );
    assert_eq!(
        field(&ada, NOTES_APP, &at, "Note", "title"),
        Value::text("Groceries"),
        "Bo's rename stays"
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

/// The runs of a change side drawn marked `name` (an insertion) or struck (a deletion), by
/// the text they cover: the rich text one side of a change holds, with what it marks.
fn marked(side: &UiNode, name: &str) -> Vec<String> {
    let Some(Value::Rich(rich)) = side.props.get("value") else {
        panic!("a rich text side: {side:#?}");
    };
    let mut out = Vec::new();
    for block in &rich.blocks {
        for mark in block.marks.iter().filter(|mark| mark.name == name) {
            out.push(
                block
                    .text
                    .chars()
                    .skip(mark.start)
                    .take(mark.end.saturating_sub(mark.start))
                    .collect(),
            );
        }
    }
    out
}

/// A workspace whose people read as their names, and a project of Ada's to file tasks into. A
/// task made here is Ada's, and Bo changes it once, so an entry of one change says what it did.
fn named_workspace_with_a_project() -> (Replica, Value, Value) {
    let (mut ada, _notes, tasks) = two_people();
    ada.host.runtime().set_name_host(Named);
    let project = call(&ada, "ada", TASKS_APP, "new_project", vec![tasks.clone()]);
    ada.commit();
    (ada, tasks, project)
}

/// A task of `title` made by Ada in the workspace's tasks.
fn a_task_of(ada: &mut Replica, tasks: &Value, title: &str) -> Value {
    let task = call(
        ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(tasks, title),
    );
    ada.commit();
    task
}

/// Words a change to the day a task is due reads with, beside the day it wrote there.
#[test]
fn a_history_entry_says_the_day_a_task_moved_to_or_was_taken_away() {
    let (mut ada, tasks, _project) = named_workspace_with_a_project();
    let set = a_task_of(&mut ada, &tasks, "Buy oat milk");
    call(
        &ada,
        "bo",
        TASKS_APP,
        "schedule",
        vec![set.clone(), Value::some(Value::text("2026-11-02"))],
    );
    ada.commit();
    let said = says(&history_of(&ada, &target(&set)));
    assert!(
        said.join(" ").contains("Bo set the due date to"),
        "a day a task did not have is set: {said:?}"
    );
    assert!(said.contains(&"2026-11-02".to_owned()), "{said:?}");

    let moved = a_task_of(&mut ada, &tasks, "Water the plants");
    call(
        &ada,
        "bo",
        TASKS_APP,
        "schedule",
        vec![moved.clone(), Value::some(Value::text("2026-11-01"))],
    );
    ada.commit();
    call(&ada, "bo", TASKS_APP, "due_tomorrow", vec![moved.clone()]);
    ada.commit();
    let said = says(&history_of(&ada, &target(&moved)));
    assert!(
        said.join(" ").contains("Bo moved it to"),
        "a day a task had is moved to another: {said:?}"
    );
    assert!(said.contains(&"2026-10-31".to_owned()), "{said:?}");
    call(&ada, "bo", TASKS_APP, "due_clear", vec![moved.clone()]);
    ada.commit();
    let said = says(&history_of(&ada, &target(&moved)));
    assert!(
        said.join(" ").contains("Bo cleared the due date"),
        "a day taken away is said so: {said:?}"
    );
}

/// Words a change to a task's priority, its project and its person reads with.
#[test]
fn a_history_entry_says_the_priority_the_project_and_the_person_a_task_moved_to() {
    let (mut ada, tasks, project) = named_workspace_with_a_project();
    let urgent = a_task_of(&mut ada, &tasks, "Pay the rent");
    call(
        &ada,
        "bo",
        TASKS_APP,
        "set_priority_high",
        vec![urgent.clone()],
    );
    ada.commit();
    let said = says(&history_of(&ada, &target(&urgent)));
    assert!(said.join(" ").contains("Bo set priority to"), "{said:?}");
    assert!(said.contains(&"High".to_owned()), "{said:?}");

    let filed = a_task_of(&mut ada, &tasks, "Book the van");
    call(
        &ada,
        "bo",
        TASKS_APP,
        "file",
        vec![filed.clone(), project.clone()],
    );
    ada.commit();
    let said = says(&history_of(&ada, &target(&filed)));
    assert!(
        said.join(" ").contains("Bo moved it to"),
        "a task put in a project is moved to it: {said:?}"
    );
    assert!(said.contains(&"New project".to_owned()), "{said:?}");

    let unfiled = a_task_of(&mut ada, &tasks, "Pack the boxes");
    call(
        &ada,
        "bo",
        TASKS_APP,
        "file",
        vec![unfiled.clone(), project],
    );
    ada.commit();
    call(&ada, "bo", TASKS_APP, "unfile", vec![unfiled.clone()]);
    ada.commit();
    let said = says(&history_of(&ada, &target(&unfiled)));
    assert!(
        said.join(" ").contains("Bo moved it out of"),
        "a task taken out of a project says the one it left: {said:?}"
    );

    let assigned = a_task_of(&mut ada, &tasks, "Call the plumber");
    call(
        &ada,
        "bo",
        TASKS_APP,
        "assign",
        vec![
            assigned.clone(),
            Value::some(Value::Principal(tilolang_runtime::Principal::new("ada"))),
        ],
    );
    ada.commit();
    let said = says(&history_of(&ada, &target(&assigned)));
    assert!(
        said.join(" ").contains("Bo assigned it to"),
        "a task given to someone says who: {said:?}"
    );
    assert!(
        said.contains(&"you".to_owned()),
        "the person looking is the one it was given to, and reads as you: {said:?}"
    );
}

/// Words a change to a field reads with, where the field's value is the whole of what it says:
/// a task checked off, one moved in the outline, one renamed.
#[test]
fn a_history_entry_says_a_task_was_completed_reopened_or_moved_without_showing_a_value() {
    let (mut ada, _notes, tasks) = two_people();
    let done = call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Write the launch post"),
    );
    ada.commit();
    call(&ada, "bo", TASKS_APP, "check", vec![done.clone()]);
    ada.commit();
    let checked = says(&history_of(&ada, &target(&done)));
    assert!(
        checked.join(" ").contains("bo marked it done"),
        "checking a task off says so, and shows no value of a flag's: {checked:?}"
    );
    call(&ada, "bo", TASKS_APP, "check", vec![done.clone()]);
    ada.commit();
    let reopened = says(&history_of(&ada, &target(&done)));
    assert!(
        reopened.join(" ").contains("bo unmarked it done"),
        "opening it again says so the other way: {reopened:?}"
    );
    assert_ne!(checked, reopened, "the check and the reopening read unlike");

    let above = call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Plan the week"),
    );
    let under = call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Outline"),
    );
    ada.commit();
    call(
        &ada,
        "bo",
        TASKS_APP,
        "indent",
        vec![under.clone(), Value::some(above)],
    );
    ada.commit();
    let said = says(&history_of(&ada, &target(&under)));
    assert!(
        said.join(" ").contains("bo moved it to"),
        "a task put under another says what it moved to: {said:?}"
    );
    assert!(
        said.contains(&"Plan the week".to_owned()),
        "the parent it moved to is shown: {said:?}"
    );
    call(&ada, "bo", TASKS_APP, "outdent", vec![under.clone()]);
    ada.commit();
    let said = says(&history_of(&ada, &target(&under)));
    assert!(
        said.join(" ").contains("bo moved it out of"),
        "a task taken out from under another says what it left: {said:?}"
    );
}

/// A board of tasks whose window shows its history: one change may write two of its fields.
const SENTENCES: &str = r#"app so.tilo.test.sentences "Sentences"

type Task {
    title: Text
    due: Option<Date>
    at: Option<Time>
}

type Tasks {
    items: Collection<Task>
}

action new_task(tasks: Tasks, title: Text, at: Option<Time>) -> Task {
    tasks.items.insert(Task(title: title, at: at))
}

/// Renames a task, and moves it to a day, in one change.
action retitle_and_move(t: Task, title: Text, due: Date) {
    t.title = title
    t.due = .some(due)
}

root Tasks

default view tasks_page for Tasks

default view task_page for Task

view tasks_page(tasks: Tasks) {
    page {
        ui.Text("Tasks")
    }
}

view task_page(t: Task) {
    page {
        ui.Text(t.title)
    }
    std.history(t)
}
"#;
const SENTENCES_APP: &str = "so.tilo.test.sentences";

/// A change that wrote two fields reads as one sentence, and each field shows what it held. The
/// window shows the task's history, as an app's does.
#[test]
fn a_change_that_wrote_two_fields_reads_as_one_sentence() {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(SENTENCES, "")]);
    assert_eq!(packages.len(), 1);
    ada.commit();
    let tasks = Value::resource(&ada.host.root_of(SENTENCES_APP).unwrap());
    let task = call(
        &ada,
        "ada",
        SENTENCES_APP,
        "new_task",
        vec![
            tasks,
            Value::text("Call the landlord"),
            Value::some(Value::text("2026-06-30T09:00:00Z")),
        ],
    );
    ada.commit();
    let at = target(&task);
    // Two fields written by one change read as one sentence, the second going on with "and".
    call(
        &ada,
        "bo",
        SENTENCES_APP,
        "retitle_and_move",
        vec![
            task,
            Value::text("Call the landlord about the boiler"),
            Value::text("2026-07-02"),
        ],
    );
    ada.commit();
    let said = says(&history_of(&ada, &at));
    assert!(
        said.join(" ").contains("bo changed title"),
        "the first field the change wrote: {said:?}"
    );
    assert!(
        said.contains(&"and set the due date to".to_owned()),
        "the second goes on with and: {said:?}"
    );
    assert!(
        said.contains(&"Call the landlord about the boiler".to_owned()),
        "the first field's value is shown beside its words: {said:?}"
    );
    assert!(said.contains(&"2026-07-02".to_owned()), "{said:?}");
}

#[test]
fn two_people_typing_one_rich_body_each_entry_shows_its_authors_change() {
    // A board of posts whose body is rich text, and whose window shows the post's history
    // under it (`tests/richtext.tl`).
    const RICHTEXT: &str = include_str!("../../../lang/tilolang/tests/fixtures/richtext.tl");
    const RICHTEXT_APP: &str = "so.tilo.lang.richtext";
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(RICHTEXT, "")]);
    let ids = packages[0].ids.clone();
    ada.commit();
    let board = target(&Value::resource(&ada.host.root_of(RICHTEXT_APP).unwrap()));
    let post = call(
        &ada,
        "ada",
        RICHTEXT_APP,
        "say_text",
        vec![Value::resource(&board), Value::text("Oat milk and bread")],
    );
    ada.commit();
    let at = target(&post);
    let mut bo = ada.fork("bo", &[(RICHTEXT, &ids)]);
    // Bo replaces a word of what Ada wrote, in her own replica of the workspace.
    let body = |text: &str| Value::Rich(tilolang_runtime::rich::RichValue::from_text(text));
    let page = view_id(&bo);
    let editors = bo.host.open_resource(&at, Some(&page)).unwrap();
    let body_field = body_editor(&editors);
    editors
        .edit(&body_field, &body("Oat milk and sourdough"))
        .unwrap();
    bo.commit();
    drop(editors);
    ada.sync_with(&mut bo);

    // Each entry says what its own author typed and what they took away, and nothing of the
    // other's change.
    let page = view_id(&ada);
    let mut editor = ada.host.open_resource(&at, Some(&page)).unwrap();
    let section = history(&mut editor);
    assert_eq!(
        entries(&section),
        ["bo", "ada"],
        "newest first, one per person"
    );
    let bodies: Vec<(UiNode, UiNode)> = changes(&section)
        .into_iter()
        .filter(|(label, ..)| label == "Body")
        .map(|(_, before, after)| (before, after))
        .collect();
    // Ada's step made the post, which the entry says in words rather than field by field;
    // Bo's step is the one body change there is to show.
    assert_eq!(bodies.len(), 1, "Bo's change of the body: {bodies:?}");
    let (showed, holds) = &bodies[0];
    assert_eq!(marked(holds, "strong"), ["sourdough"]);
    assert_eq!(marked(showed, "strike"), ["bread"]);
}

/// The id of the `richtext` package's view `name`, as it declares it.
fn view_id(r: &Replica) -> String {
    r.host
        .package("so.tilo.lang.richtext")
        .unwrap()
        .meta
        .views
        .iter()
        .find(|v| v.name == "post_edit")
        .map(|v| v.id.clone())
        .expect("the richtext package has a post_edit view")
}

/// The role assistive technology reads a node as, as the component drawing it describes
/// itself (the body's document, a field, a button): what a person is told of a control, never
/// the kind string of the widget behind it, which the toolkit changes as its widgets do.
fn access_role(node: &UiNode) -> Option<&'static str> {
    let name = node.kind.strip_prefix(tilolang::toolkit::KIND_PREFIX)?;
    Some(
        tilolang::toolkit::catalogue()
            .get(name)?
            .semantics
            .role
            .as_str(),
    )
}

/// The key of the window's body: the document a person types the post's body into.
///
/// It is the node assistive technology reads as a document that the view bound to the post's
/// field; the history's own rich text is read-only, so it binds nothing.
fn body_editor(view: &ViewInstance) -> String {
    nodes(view.tree())
        .into_iter()
        .find(|node| node.binding.is_some() && access_role(node) == Some("Document"))
        .map_or_else(
            || panic!("the window binds the post's body: {:#?}", view.tree()),
            |node| node.key.clone(),
        )
}

/// A post of `paragraphs` paragraphs that all read the same, which Ada wrote on her board, and
/// Bo's replica of the workspace holding it: a long note, as two people each have it.
struct LongPost {
    ada: Replica,
    bo: Replica,
    at: ResourceRef,
    body: tilolang_runtime::store::FieldSpec,
    paragraphs: usize,
}

/// The paragraph every paragraph of a [`LongPost`] starts as.
const PARAGRAPH: &str = "The quick brown fox jumps over the lazy dog and keeps on going. ";

impl LongPost {
    fn new(paragraphs: usize) -> Self {
        const RICHTEXT: &str = include_str!("../../../lang/tilolang/tests/fixtures/richtext.tl");
        const RICHTEXT_APP: &str = "so.tilo.lang.richtext";
        let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(RICHTEXT, "")]);
        let ids = packages[0].ids.clone();
        ada.commit();
        let board = target(&Value::resource(&ada.host.root_of(RICHTEXT_APP).unwrap()));
        let body = tilolang_runtime::store::FieldSpec {
            id: crate::fixtures::field_id(
                &ada.host.package(RICHTEXT_APP).unwrap().meta,
                "Post",
                "body",
            ),
            kind: tilolang_runtime::meta::FieldKind::RichText,
        };
        let post = call(
            &ada,
            "ada",
            RICHTEXT_APP,
            "say_text",
            vec![
                Value::resource(&board),
                Value::text(vec![PARAGRAPH; paragraphs].join("\n")),
            ],
        );
        ada.commit();
        let at = target(&post);
        let bo = ada.fork("bo", &[(RICHTEXT, &ids)]);
        Self {
            ada,
            bo,
            at,
            body,
            paragraphs,
        }
    }

    /// The paragraphs of the post's body as `replica` reads it.
    fn reads(&self, replica: &Replica) -> Vec<String> {
        match replica.host.store().read(&self.at, &self.body).unwrap() {
            tilolang_runtime::store::FieldValue::Rich(rich) => {
                rich.blocks.into_iter().map(|block| block.text).collect()
            }
            other => panic!("the body is rich text: {other:?}"),
        }
    }

    /// `who` (Ada or Bo) replaces `word` with `with` in the paragraph `at`, in the post's window
    /// on their own replica, as its editor writes: the body as they read it, with that one
    /// paragraph typed into.
    fn type_into(&mut self, bo: bool, at: usize, word: &str, with: &str) {
        let mut lines = self.reads(if bo { &self.bo } else { &self.ada });
        assert_eq!(lines.len(), self.paragraphs, "every paragraph is there");
        let line = lines.get_mut(at).unwrap();
        *line = line.replacen(word, with, 1);
        let value = Value::Rich(tilolang_runtime::rich::RichValue::from_text(
            &lines.join("\n"),
        ));
        let replica = if bo { &mut self.bo } else { &mut self.ada };
        let page = view_id(replica);
        let editors = replica.host.open_resource(&self.at, Some(&page)).unwrap();
        let body_field = body_editor(&editors);
        editors.edit(&body_field, &value).unwrap();
        drop(editors);
        replica.commit();
    }
}

/// The work reading one entry's body diff does in a post of `paragraphs` paragraphs: the
/// store's reads of the record and the document's operations Automerge searches, counted.
///
/// The body is written before any window shows it: a window draws the whole body, which is
/// the post's length, and that is not the entry's read.
fn entry_work(paragraphs: usize) -> (u64, u64, usize) {
    use tilolang_runtime::store::Store as _;
    let mut post = LongPost::new(paragraphs);
    let chars = PARAGRAPH.chars().count().saturating_mul(paragraphs);
    // Bo types one word in the first paragraph, in his own replica, and it reaches Ada's.
    post.type_into(true, 0, "quick", "swift");
    let LongPost {
        mut ada,
        mut bo,
        at,
        body: spec,
        ..
    } = post;
    ada.sync_with(&mut bo);
    // Reading the entry's diff: the change Bo made, as the history reads it.
    let change = ada
        .host
        .store()
        .field_changed(&at, &spec.id)
        .unwrap()
        .expect("the change that wrote the body");
    assert_eq!(
        change.who, "bo",
        "the body was last changed by Bo's typing: {change:?}"
    );
    let change = change.change;
    let before = ada.host.store().read_stats().doc;
    let work = automerge::work();
    let written = ada
        .host
        .store()
        .written(&at, std::slice::from_ref(&spec), &change)
        .unwrap();
    let after = ada.host.store().read_stats().doc;
    let done = automerge::work();
    assert_eq!(written.len(), 1, "the entry shows the body's change");
    // What it shows is the paragraph Bo typed into, cut to the words around the change: the
    // word he took away and the one he wrote, never the note.
    let said = |value: &tilolang_runtime::store::FieldValue| match value {
        tilolang_runtime::store::FieldValue::Rich(rich) => rich
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => panic!("a rich text's change: {other:?}"),
    };
    let (_, was, now) = &written[0];
    let (was, now) = (said(was), said(now));
    assert!(
        was.contains("quick") && !was.contains("swift") && now.contains("swift"),
        "the entry shows the word Bo replaced: {was:?} -> {now:?}"
    );
    assert!(
        was.len() <= PARAGRAPH.len() && now.len() <= PARAGRAPH.len(),
        "the entry shows one paragraph at most: {was:?} -> {now:?}"
    );
    let reads = after
        .fields
        .saturating_sub(before.fields)
        .saturating_add(after.entries.saturating_sub(before.entries))
        .saturating_add(after.listings.saturating_sub(before.listings));
    // The change graph is walked for nothing either: no ancestor is visited.
    assert_eq!(
        done.visited_changes.saturating_sub(work.visited_changes),
        0,
        "reading one entry walks no history"
    );
    (
        reads,
        done.searched_slabs.saturating_sub(work.searched_slabs),
        chars,
    )
}

#[test]
fn two_people_typing_into_different_paragraphs_of_a_long_note_at_once_both_keep_their_words() {
    // A note past a hundred thousand characters: what its editor writes on each keystroke is
    // the whole body, which its change records with the body kept as content.
    let mut post = LongPost::new(1_571);
    // Each types into their own copy before either hears of the other.
    post.type_into(true, 0, "quick", "swift");
    post.type_into(false, 1_200, "lazy", "sleepy");
    let (ada_wrote, bo_wrote) = (post.reads(&post.ada), post.reads(&post.bo));
    assert!(ada_wrote[1_200].contains("sleepy") && bo_wrote[0].contains("swift"));
    let LongPost {
        mut ada, mut bo, ..
    } = post;
    ada.sync_with(&mut bo);
    let post = LongPost { ada, bo, ..post };
    for (who, replica) in [("Ada", &post.ada), ("Bo", &post.bo)] {
        let lines = post.reads(replica);
        assert_eq!(lines.len(), 1_571, "{who} reads every paragraph once");
        assert!(
            lines[0].contains("swift") && !lines[0].contains("quick"),
            "{who} reads Bo's word: {:?}",
            lines[0]
        );
        assert!(
            lines[1_200].contains("sleepy") && !lines[1_200].contains(" lazy"),
            "{who} reads Ada's word: {:?}",
            lines[1_200]
        );
        let untouched = lines
            .iter()
            .enumerate()
            .filter(|(at, _)| *at != 0 && *at != 1_200)
            .all(|(_, line)| line == PARAGRAPH.trim_end() || line == PARAGRAPH);
        assert!(untouched, "{who} reads every other paragraph as it was");
    }
}

#[test]
fn reading_one_entrys_body_diff_stays_within_a_fixed_budget_however_long_the_note() {
    let (small_reads, small_searches, small_chars) = entry_work(40);
    // A note past a hundred thousand characters.
    let (large_reads, large_searches, large_chars) = entry_work(1_571);
    assert!(
        small_chars >= 2_000 && large_chars >= 100_000,
        "a small note and one past a hundred thousand characters: {small_chars}, {large_chars}"
    );
    // The same work in a note forty times the length, to within the places of a handful of
    // objects: what reading the entry costs is the paragraph it changed, never the note.
    assert_eq!(
        large_reads, small_reads,
        "the store reads nothing of the record either way: {small_reads} reads in {small_chars} \
         characters, {large_reads} in {large_chars}"
    );
    assert!(
        large_searches <= small_searches,
        "the document searched: {small_searches} times in {small_chars} characters, \
         {large_searches} in {large_chars}"
    );
}

/// The workspace calls Ada "Ada Lovelace"; an agent has no name but its kind. The history row shows
/// the name the avatar beside it shows, never the id the record holds, and the agent reads as
/// its kind and that it is one.
struct Named;

impl tilolang_tilo::runtime::NameHost for Named {
    fn name(&self, who: &tilolang_tilo::runtime::Principal) -> String {
        match who.0.as_str() {
            "ada" => "Ada Lovelace".to_owned(),
            "bo" => "Bo".to_owned(),
            other => tilolang_tilo::names::unnamed(other),
        }
    }
}

/// Everything a history section says, top to bottom: each entry's sentence, then what each
/// change shows of the field it wrote.
fn says(section: &UiNode) -> Vec<String> {
    nodes(section)
        .into_iter()
        .filter_map(text)
        .map(str::to_owned)
        .collect()
}

/// What a section of history reads as: its texts in the order the rows show them, so an entry
/// reads as who made it and, beside the words of what it did, what was done. A row's buttons
/// (`Undo`, a flag's yes and no) stand in the row but are not part of what the entry says.
fn reads_as(section: &UiNode) -> String {
    nodes(section)
        .into_iter()
        .filter(|node| node.kind == "ui.Text")
        .filter_map(text)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The history section of a window on `at`.
fn history_of(ada: &Replica, at: &ResourceRef) -> UiNode {
    let mut page = ada.host.open_resource(at, None).unwrap();
    history(&mut page)
}

/// What the node showing each entry's who says, top to bottom: the label text beside the
/// avatar, as the row shows it.
fn labels(section: &UiNode) -> Vec<String> {
    nodes(section)
        .into_iter()
        .filter(|node| node.kind == "ui.Text")
        .filter_map(|node| match node.props.get("role") {
            Some(Value::Text(role)) if role == "label" => text(node).map(str::to_owned),
            _ => None,
        })
        .collect()
}

#[test]
fn a_history_row_names_the_person_as_the_avatar_does_and_an_agent_as_its_name_and_kind() {
    let (mut ada, notes, _) = two_people();
    ada.host.runtime().set_name_host(Named);
    let note = call(&ada, "ada", NOTES_APP, "new_note", vec![notes]);
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
        AGENT,
        NOTES_APP,
        "retitle",
        vec![note.clone(), Value::text("Shopping")],
    );
    ada.commit_in_run(AGENT, RUN, "Tidy the notes");
    let mut editor = ada.host.open_resource(&target(&note), None).unwrap();
    let section = history(&mut editor);
    // The run's rename, newest, then Ada's own: the agent reads as its name plus that it is
    // software, the person looking as the platform's word for them, each over what it did.
    assert_eq!(
        labels(&section),
        ["Helper (agent)", "You", "changed title"],
        "the rows name people as the avatars do, never their ids"
    );
    assert!(
        !labels(&section)
            .iter()
            .any(|label| label == "ada" || label == AGENT),
        "no row shows a participant's id"
    );
}

/// The key of the node bound to `field` in a window.
fn bound_key(node: &UiNode, field: &str) -> Option<String> {
    if node
        .binding
        .as_ref()
        .and_then(|binding| binding.field())
        .is_some_and(|(_, bound)| bound == field)
    {
        return Some(node.key.clone());
    }
    node.children
        .iter()
        .find_map(|child| bound_key(child, field))
}

#[test]
fn typing_in_a_bound_field_reads_as_an_edit_of_that_field() {
    let (mut ada, _notes, tasks) = two_people();
    let task = call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Buy oat milk"),
    );
    ada.commit();
    let at = target(&task);
    let title =
        crate::fixtures::field_id(&ada.host.package(TASKS_APP).unwrap().meta, "Task", "title");
    let page = ada.host.open_resource(&at, None).unwrap();
    let input = bound_key(page.tree(), &title).expect("the task's title is bound to its input");
    page.edit(&input, &Value::text("Bring oat milk")).unwrap();
    ada.commit();
    drop(page);

    // The entry says what was done, in the words of the field it wrote and the value it wrote
    // there, and never the platform's verb id.
    let mut page = ada.host.open_resource(&at, None).unwrap();
    let section = history(&mut page);
    let said = says(&section);
    assert!(
        said.join(" ").contains("You changed title"),
        "the entry says what was done: {said:?}"
    );
    page.press(&button(&section, "tilolang-history-show"))
        .unwrap();
    let section = history(&mut page);
    let said = says(&section);
    assert!(
        said.contains(&"Bring oat milk".to_owned()),
        "the entry opened says the name it took: {said:?}"
    );
    assert!(
        !said.iter().any(|what| what.contains("dit-text")),
        "no entry shows an action id: {said:?}"
    );
}

/// Checking a task off is one entry of history, saying in a sentence who did it and what it
/// did: it took the tasks under it with it, and the step before it is an entry of its own.
#[test]
fn checking_a_task_off_is_one_entry_saying_what_it_did() {
    const SAYS: &str = "You marked it done";
    let (mut ada, _, tasks) = two_people();
    let parent = call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Write the launch post"),
    );
    call(
        &ada,
        "ada",
        TASKS_APP,
        "add_subtask",
        vec![tasks, parent.clone(), Value::text("Outline")],
    );
    ada.commit();
    // Bo puts it off a day, so Ada's check stands after a step of his.
    call(
        &ada,
        "bo",
        TASKS_APP,
        "schedule",
        vec![parent.clone(), Value::some(Value::text("2026-11-02"))],
    );
    ada.commit();
    call(&ada, "ada", TASKS_APP, "check", vec![parent.clone()]);
    ada.commit();

    let mut page = ada.host.open_resource(&target(&parent), None).unwrap();
    let section = history(&mut page);
    assert_eq!(
        entries(&section),
        ["ada", "bo", "ada"],
        "newest first: the check, then Bo's step, then the making"
    );
    assert!(
        reads_as(&section).contains(SAYS),
        "the check's entry says who checked it and what it did: {section:#?}"
    );
    let (label, ..) = changes(&section)
        .into_iter()
        .next()
        .expect("the check's change is shown");
    assert_eq!(label, "Done");
}

/// The Undo button beside the version that says `said`.
fn undo_saying(section: &UiNode, said: &str) -> String {
    let found: Vec<&UiNode> = nodes(section)
        .into_iter()
        .filter(|node| node.kind == "row")
        .filter(|row| {
            let held = nodes(row);
            held.iter()
                .any(|child| text(child).is_some_and(|t| t.contains(said)))
                && held
                    .iter()
                    .any(|child| child.kind == "ui.Button" && text(child) == Some("tilolang-undo"))
        })
        .collect();
    narrowest(found)
        .and_then(|row| {
            nodes(row)
                .into_iter()
                .find(|child| child.kind == "ui.Button" && text(child) == Some("tilolang-undo"))
        })
        .map_or_else(
            || panic!("a change saying {said}, with its Undo: {section:#?}"),
            |button| button.key.clone(),
        )
}

/// One command over six chosen tasks is one change: each task stands in it, the entry says how
/// many it changed, and its Undo takes all six back together.
#[test]
fn a_command_over_many_chosen_tasks_is_one_change_its_undo_takes_back() {
    const SAID: &str = "tilolang-did-many";
    let (mut ada, _, tasks) = two_people();
    let root = target(&tasks);
    let six: Vec<ResourceRef> = (1..=6)
        .map(|n| {
            target(&call(
                &ada,
                "ada",
                TASKS_APP,
                "new_task",
                crate::tasks::create(&tasks, &format!("Task {n}")),
            ))
        })
        .collect();
    ada.commit();
    let before = field(&ada, TASKS_APP, &six[0], "Task", "due");
    let mut window = ada.host.open_resource(&root, None).unwrap();
    // The window opens on Today, where an undated task is not: the six wait in the Inbox.
    crate::tasks::go_to(&mut window, "Inbox");
    let list = nodes(window.tree())
        .into_iter()
        .find(|node| node.kind == "ui.VirtualList")
        .expect("the window shows its tasks in a list")
        .key
        .clone();
    let items: Vec<Value> = six.iter().map(Value::resource).collect();
    window
        .fire_with(&list, "selected", Some(&Value::List(items)))
        .unwrap();
    window.poll_patches().unwrap();
    // The bar offers what every chosen task takes, once over all of them.
    let today = button(window.tree(), "Today");
    window.press(&today).unwrap();
    window.poll_patches().unwrap();
    ada.commit();

    // Each of the six stands in the same change, which was made over six items.
    let mut changes = Vec::new();
    for task in &six {
        let versions = ada.host.store().versions(task).unwrap();
        let newest = versions.last().expect("the task's change");
        assert_eq!(newest.count, 6, "the change says it was made over six");
        changes.push(newest.change.clone());
        assert_ne!(
            field(&ada, TASKS_APP, task, "Task", "due"),
            before,
            "the command reached every chosen task"
        );
    }
    assert!(
        changes.windows(2).all(|pair| pair[0] == pair[1]),
        "the six are one change: {changes:?}"
    );

    // The task's page says what happened in one entry: the words for a change over many
    // items, which the platform's catalogue reads with the count (this session holds no
    // words, so they stand as their key), not the words of what it did to this one task.
    let mut page = ada.host.open_resource(&six[0], None).unwrap();
    let section = history(&mut page);
    assert!(
        says(&section).iter().any(|said| said.contains(SAID)),
        "the entry says it changed many, counting them: {:?}",
        says(&section)
    );
    page.press(&button(&section, "tilolang-history-show"))
        .unwrap();
    let section = history(&mut page);
    // Its own Undo takes back the one change, and with it every task it changed.
    let asked = press_undo(&mut ada, &mut page, &undo_saying(&section, SAID));
    assert!(
        matches!(asked, UndoAsk::Step { .. }),
        "one change is one step: {asked:?}"
    );
    for task in &six {
        assert_eq!(
            field(&ada, TASKS_APP, task, "Task", "due"),
            before,
            "Undo takes every task the change reached back with it"
        );
    }
}

/// Whether the task `at` is done.
fn done(r: &Replica, at: &ResourceRef) -> Value {
    field(r, TASKS_APP, at, "Task", "done")
}

/// How many tasks the list `root` holds.
fn held(r: &Replica, root: &ResourceRef) -> usize {
    match r.host.runtime().read(root, "items").unwrap() {
        Value::List(items) => items.len(),
        other => panic!("a list of tasks: {other:?}"),
    }
}

/// How many changes the workspace holds of `roots`: of each resource in it.
fn changes_of(r: &Replica, roots: &[&ResourceRef]) -> usize {
    roots
        .iter()
        .map(|root| r.host.store().versions(root).unwrap().len())
        .sum()
}

/// An action that calls two of its app's own actions is one step of the person's: one Undo
/// takes back every change it made, and one Redo brings them all back.
#[test]
fn an_action_that_calls_two_of_its_apps_actions_is_one_step() {
    let (mut ada, _notes, tasks) = two_people();
    let root = target(&tasks);
    let parent = target(&call(
        &ada,
        "ada",
        TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Parent"),
    ));
    call(
        &ada,
        "ada",
        TASKS_APP,
        "check",
        vec![Value::resource(&parent), Value::Bool(true)],
    );
    ada.commit();
    assert_eq!(done(&ada, &parent), Value::Bool(true));
    let before = changes_of(&ada, &[&root, &parent]);

    // A subtask under the task that was done: the action inserts it and calls `check` to open
    // the task again, so the person's one press made two changes.
    call(
        &ada,
        "ada",
        TASKS_APP,
        "add_subtask",
        vec![tasks, Value::resource(&parent), Value::text("Child")],
    );
    ada.commit();
    assert_eq!(
        done(&ada, &parent),
        Value::Bool(false),
        "the subtask opened the task again"
    );
    assert_eq!(held(&ada, &root), 2, "and it stands in the list");
    assert_eq!(
        changes_of(&ada, &[&root, &parent]),
        before + 2,
        "the one action made two changes"
    );

    // One Undo takes the whole action back: the subtask goes, and the task is done again.
    ada.history(&HistoryAsk::Undo.edit());
    assert_eq!(held(&ada, &root), 1, "Undo took the subtask back");
    assert_eq!(
        done(&ada, &parent),
        Value::Bool(true),
        "and made the task done again"
    );

    // One Redo brings both of them back.
    ada.history(&HistoryAsk::Redo.edit());
    assert_eq!(held(&ada, &root), 2, "Redo brought the subtask back");
    assert_eq!(
        done(&ada, &parent),
        Value::Bool(false),
        "and opened the task again"
    );
}

/// An app whose action calls two of its own actions, each changing the one record: what makes
/// the action's changes two, of one person, on one resource.
const PAIRED: &str = r#"app test.paired "Paired"

type Tally {
  value: Int merge counter
  note: Text
}

root Tally

default view tally for Tally

view tally(t: Tally) {
  std.page(t) {
    ui.Text("{t.value}")
    std.history(t)
  }
}

action bump(t: Tally) {
  t.value += 1
}

action note(t: Tally) {
  t.note = "noted"
}

action both(t: Tally) {
  bump(t)
  note(t)
}
"#;

const PAIRED_APP: &str = "test.paired";

/// The changes one action made on one record are one entry of its history, and its Undo takes
/// them all back together.
#[test]
fn the_changes_of_one_action_are_one_entry_of_history_its_undo_takes_back() {
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(PAIRED, "")]);
    let tally = ada.host.root_of(PAIRED_APP).unwrap();
    ada.commit();
    let before = changes_of(&ada, &[&tally]);
    call(
        &ada,
        "ada",
        PAIRED_APP,
        "both",
        vec![Value::resource(&tally)],
    );
    ada.commit();
    assert_eq!(
        changes_of(&ada, &[&tally]),
        before + 2,
        "the action and the two it called are two changes"
    );

    // History lists the action once: one entry saying how many changes it made, beside the
    // record's own first entry.
    let mut window = ada.host.open_resource(&tally, None).unwrap();
    let section = history(&mut window);
    let said = says(&section);
    assert_eq!(
        entries(&section).len(),
        2,
        "the record's creation, then the action: {said:#?}"
    );
    assert_eq!(
        said.iter()
            .filter(|said| said.as_str() == "tilolang-history-changes")
            .count(),
        1,
        "the action stands in history once, saying how many changes it made: {said:?}"
    );

    // One Undo takes every change of it back, and one Redo brings them back.
    ada.history(&HistoryAsk::Undo.edit());
    assert_eq!(
        field(&ada, PAIRED_APP, &tally, "Tally", "value"),
        Value::Int(0),
        "Undo took the action's first change back"
    );
    assert_eq!(
        field(&ada, PAIRED_APP, &tally, "Tally", "note"),
        Value::text(""),
        "and its second with it"
    );
    ada.history(&HistoryAsk::Redo.edit());
    assert_eq!(
        field(&ada, PAIRED_APP, &tally, "Tally", "value"),
        Value::Int(1),
        "Redo brought the action's first change back"
    );
    assert_eq!(
        field(&ada, PAIRED_APP, &tally, "Tally", "note"),
        Value::text("noted"),
        "and its second with it"
    );
}
