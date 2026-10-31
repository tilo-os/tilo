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
        "say",
        vec![Value::resource(&board), Value::text("Oat milk and bread")],
    );
    ada.commit();
    let at = target(&post);
    let mut bo = ada.fork("bo", &[(RICHTEXT, &ids)]);
    // Bo replaces a word of what Ada wrote, in her own replica of the room.
    let body = |text: &str| Value::Rich(tilolang_runtime::rich::RichValue::from_text(text));
    let page = view_id(&bo);
    let mut editors = bo.host.open_resource(&at, Some(&page)).unwrap();
    let input = rich_input(&mut editors);
    editors
        .edit(&input, &body("Oat milk and sourdough"))
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

/// The key of the window's rich input.
fn rich_input(view: &mut ViewInstance) -> String {
    nodes(view.tree())
        .into_iter()
        .find(|n| n.kind == "ui.RichInput")
        .map(|n| n.key.clone())
        .expect("the window has a rich input")
}

/// The work reading one entry's body diff does in a post of `paragraphs` paragraphs: the
/// store's reads of the record and the document's operations Automerge searches, counted.
///
/// The body is written before any window shows it: a window draws the whole body, which is
/// the post's length, and that is not the entry's read.
fn entry_work(paragraphs: usize) -> (u64, u64, usize) {
    const RICHTEXT: &str = include_str!("../../../lang/tilolang/tests/fixtures/richtext.tl");
    const RICHTEXT_APP: &str = "so.tilo.lang.richtext";
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(RICHTEXT, "")]);
    let ids = packages[0].ids.clone();
    ada.commit();
    let board = target(&Value::resource(&ada.host.root_of(RICHTEXT_APP).unwrap()));
    let paragraph = "The quick brown fox jumps over the lazy dog and keeps on going. ";
    let chars = paragraph.chars().count() * paragraphs;
    let spec = tilolang_runtime::store::FieldSpec {
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
        "say",
        vec![
            Value::resource(&board),
            Value::text(&vec![paragraph; paragraphs].join("\n")),
        ],
    );
    ada.commit();
    let at = target(&post);
    // Bo types one word in the first paragraph, in her own replica, and it reaches Ada's.
    let body = |text: &str| Value::Rich(tilolang_runtime::rich::RichValue::from_text(text));
    let mut bo = ada.fork("bo", &[(RICHTEXT, &ids)]);
    let swift = paragraph.replacen("quick", "swift", 1);
    let mut typed = vec![paragraph; paragraphs];
    if let Some(first) = typed.first_mut() {
        *first = &swift;
    }
    let page = view_id(&bo);
    let mut editors = bo.host.open_resource(&at, Some(&page)).unwrap();
    let input = rich_input(&mut editors);
    editors.edit(&input, &body(&typed.join("\n"))).unwrap();
    bo.commit();
    drop(editors);
    ada.sync_with(&mut bo);
    // Reading the entry's diff: the change Bo made, as the history reads it.
    use tilolang_runtime::store::Store as _;
    let change = bo
        .doc
        .get_changes(&[])
        .last()
        .map(|change| change.hash().to_string())
        .expect("Bo's change");
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
    let reads = (after.fields - before.fields)
        + (after.entries - before.entries)
        + (after.listings - before.listings);
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

/// The room calls Ada "Ada Lovelace"; an agent has no name but its kind. The history row shows
/// the name the avatar beside it shows, never the id the record holds, and the agent reads as
/// its kind and that it is one.
struct Named;

impl tilolang_tilo::runtime::NameHost for Named {
    fn name(&self, who: &tilolang_tilo::runtime::Principal) -> String {
        match who.0.as_str() {
            "ada" => "Ada Lovelace".to_owned(),
            other => tilolang_tilo::names::unnamed(other),
        }
    }
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
    // The run's rename, newest, then Ada's own: the row reads the name the avatar shows, and
    // the agent as its name plus that it is software.
    assert_eq!(
        labels(&section),
        ["Helper (agent)", "Ada Lovelace"],
        "the rows name people as the avatars do, never their ids"
    );
    assert!(
        !labels(&section)
            .iter()
            .any(|label| label == "ada" || label == AGENT),
        "no row shows a participant's id"
    );
}
