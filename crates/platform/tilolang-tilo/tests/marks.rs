//! Marks in place (task-legibility.md §3): with no code of the app's, the host marks the rows,
//! the bound fields and the window of what an agent's run changed and the person has not seen;
//! each mark shows the run that made it; a view may place the mark itself; and every mark goes
//! once the run is seen.

use automerge::AutoCommit;
use tilolang_runtime::{Principal, ResourceRef, UiNode, Value, ViewEvent, ViewInstance};

use crate::fixtures::Replica;
use crate::legibility::{AGENT, NOTES_APP, RUN, Tidied, a_run_in_notes_and_tasks, call};

fn find_all<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        find_all(child, kind, out);
    }
}

fn all<'a>(node: &'a UiNode, kind: &str) -> Vec<&'a UiNode> {
    let mut out = Vec::new();
    find_all(node, kind, &mut out);
    out
}

/// Every row of the view's list, as the list builds them.
fn rows(view: &mut ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let list = all(view.tree(), "ui.VirtualList")
        .first()
        .expect("a list")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|i| view.row(&list, i).unwrap().cloned())
        .collect()
}

/// The resource each row presents, and the change mark the host placed on it, if any.
fn row_marks(view: &mut ViewInstance) -> Vec<(String, Option<UiNode>)> {
    let mut out = Vec::new();
    for row in rows(view) {
        for item in all(&row, "ui.ListItem") {
            let of = match item.props.get("item") {
                Some(Value::Ref(r)) => r.target.id_string(),
                other => panic!("a row presents its item: {other:?}"),
            };
            let mark = item
                .children
                .iter()
                .find(|c| c.kind == "slot" && c.props.get("name") == Some(&Value::text("change")))
                .and_then(|slot| slot.children.first().cloned());
            out.push((of, mark));
        }
    }
    out
}

fn principal(node: &UiNode, prop: &str) -> Option<String> {
    match node.props.get(prop) {
        Some(Value::Principal(who)) => Some(who.0.clone()),
        _ => None,
    }
}

/// Everyone the mark names, in the order it names them.
fn principals(node: &UiNode) -> Vec<String> {
    match node.props.get("by_all") {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::Principal(who) => Some(who.0.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// What the mark says the newest change did, as the line under the row's title.
fn said(node: &UiNode) -> String {
    match node.props.get("said") {
        Some(Value::Text(said)) => said.clone(),
        other => panic!("a mark says what was done: {other:?}"),
    }
}

fn marked<'m>(marks: &'m [(String, Option<UiNode>)], id: &str) -> Option<&'m UiNode> {
    marks
        .iter()
        .find(|(of, _)| of == id)
        .and_then(|(_, mark)| mark.as_ref())
}

#[test]
fn the_host_marks_rows_and_the_window_and_a_mark_shows_its_run() {
    let Tidied {
        ada,
        made,
        groceries,
        ..
    } = a_run_in_notes_and_tasks();

    // The notebook: the rows the run made or changed carry its mark, with who and which run;
    // no app code asked for it.
    let mut notebook = ada.view(NOTES_APP);
    let marks = row_marks(&mut notebook);
    for (at, kind) in [(&made, "created"), (&groceries, "changed")] {
        let mark = marked(&marks, &at.id_string())
            .unwrap_or_else(|| panic!("{at:?} is marked: {marks:?}"));
        assert_eq!(mark.kind, "ui.ChangeMark");
        assert_eq!(principal(mark, "by").as_deref(), Some(AGENT));
        assert_eq!(
            mark.props.get("run"),
            Some(&Value::text("Tidy my notes and tasks"))
        );
        assert_eq!(mark.props.get("kind"), Some(&Value::text(kind)));
    }
    // What each marked row says under its title: the change in words.
    assert_eq!(
        said(marked(&marks, &made.id_string()).unwrap()),
        "created it"
    );
    assert!(
        said(marked(&marks, &groceries.id_string()).unwrap()).contains("title"),
        "the renamed note says what changed: {}",
        said(marked(&marks, &groceries.id_string()).unwrap())
    );
    // The window's header counts what changed in it, and by whom.
    let window = all(notebook.tree(), "ui.ChangeMark")
        .into_iter()
        .find(|m| m.props.get("window") == Some(&Value::Bool(true)))
        .cloned()
        .expect("the window's header is marked");
    assert_eq!(principal(&window, "by").as_deref(), Some(AGENT));
    assert_eq!(
        principals(&window),
        vec![AGENT.to_owned()],
        "the header names everyone who changed it"
    );
    assert_eq!(
        window.props.get("noun_one"),
        Some(&Value::text("note")),
        "the header says one of them, so one change agrees in number: {window:?}"
    );
    assert_eq!(
        window.props.get("noun"),
        Some(&Value::text("notes")),
        "the header's count names what changed: {window:?}"
    );
    assert!(
        matches!(window.props.get("count"), Some(Value::Int(n)) if *n >= 2),
        "{window:?}"
    );

    // Activating a mark shows the run that made it.
    let key = marked(&marks, &groceries.id_string()).unwrap().key.clone();
    let found = notebook.find(&key).cloned();
    assert!(found.is_some(), "the mark is found by its key");
    assert!(
        found.unwrap().events.contains_key("activate"),
        "it activates"
    );
    notebook.fire(&key, "activate").unwrap();
    assert!(
        notebook.poll_events().iter().any(|event| matches!(
            event,
            ViewEvent::Review { run, target } if run == RUN && *target == groceries
        )),
        "the mark shows its run"
    );

    // Mark all seen on the run: every mark goes, live.
    assert!(ada.host.runtime().mark_run_seen("ada", RUN));
    let marks = row_marks(&mut notebook);
    assert!(marks.iter().all(|(_, mark)| mark.is_none()), "{marks:?}");
    assert!(
        all(notebook.tree(), "ui.ChangeMark").is_empty(),
        "the window's mark is gone"
    );
}

/// The window's mark names everyone who changed what it shows, newest first: not only whoever
/// happened to change it last.
#[test]
fn the_header_names_everyone_who_changed_it() {
    const BO: &str = "bo";
    const SORTED: &str = "issue-8@8000000000000001";
    let Tidied { mut ada, .. } = a_run_in_notes_and_tasks();
    let notes = Value::resource(&ada.host.root_of(NOTES_APP).unwrap());
    call(&ada, BO, NOTES_APP, "new_note", vec![notes]);
    ada.commit_in_run(BO, SORTED, "Sort the paper mail");

    let mut notebook = ada.view(NOTES_APP);
    notebook.poll_patches().unwrap();
    let window = all(notebook.tree(), "ui.ChangeMark")
        .into_iter()
        .find(|m| m.props.get("window") == Some(&Value::Bool(true)))
        .cloned()
        .expect("the window's header is marked");
    let mut who = principals(&window);
    who.sort();
    assert_eq!(
        who,
        {
            let mut expected = vec![BO.to_owned(), AGENT.to_owned()];
            expected.sort();
            expected
        },
        "the header names everyone who changed it"
    );
    assert!(
        matches!(window.props.get("count"), Some(Value::Int(n)) if *n >= 3),
        "every change is counted: {window:?}"
    );
}

/// The rows the host placed in `list` for removed items: each one's title and mark.
fn removed_rows(tree: &UiNode) -> Vec<(String, UiNode)> {
    let mut out = Vec::new();
    for list in all(tree, "ui.VirtualList") {
        let Some(slot) = list
            .children
            .iter()
            .find(|c| c.kind == "slot" && c.props.get("name") == Some(&Value::text("removed")))
        else {
            continue;
        };
        for item in &slot.children {
            assert_eq!(item.props.get("removed"), Some(&Value::Bool(true)));
            let title = all(item, "ui.ListRow")
                .first()
                .and_then(|row| row.props.get("title").cloned());
            let mark = all(item, "ui.ChangeMark").first().map(|m| (*m).clone());
            if let (Some(Value::Text(title)), Some(mark)) = (title, mark) {
                out.push((title, mark));
            }
        }
    }
    out
}

#[test]
fn a_task_the_run_removed_stays_a_struck_row_until_its_removal_is_seen() {
    let Tidied { ada, .. } = a_run_in_notes_and_tasks();
    let mut inbox = ada.view(crate::legibility::TASKS_APP);
    crate::tasks::go_to(&mut inbox, "Inbox");
    inbox.poll_patches().unwrap();
    let removed = removed_rows(inbox.tree());
    assert_eq!(removed.len(), 1, "{removed:?}");
    let (title, mark) = &removed[0];
    assert_eq!(title, "Stale", "the row says what was removed");
    assert_eq!(mark.props.get("kind"), Some(&Value::text("removed")));
    assert_eq!(principal(mark, "by").as_deref(), Some(AGENT));
    // The notebook's list shows the note it removed too; nothing else lists either.
    let mut notebook = ada.view(NOTES_APP);
    notebook.poll_patches().unwrap();
    assert_eq!(removed_rows(notebook.tree()).len(), 1);
    // Seen, the row goes.
    assert!(ada.host.runtime().mark_run_seen("ada", RUN));
    inbox.poll_patches().unwrap();
    assert!(removed_rows(inbox.tree()).is_empty());
}

/// A list built over a record that already holds the removal shows the row struck too: the
/// index knows what left each collection without reading the log.
#[test]
fn a_list_built_over_the_record_shows_what_left_it() {
    let Tidied { ada, .. } = a_run_in_notes_and_tasks();
    let fresh = crate::legibility::built_over(&ada);
    assert!(
        fresh.store().stats().builds > 0,
        "the indexes are built whole"
    );
    let mut inbox = fresh.open_main(crate::legibility::TASKS_APP).unwrap();
    crate::tasks::go_to(&mut inbox, "Inbox");
    inbox.poll_patches().unwrap();
    let removed = removed_rows(inbox.tree());
    assert_eq!(removed.len(), 1, "{removed:?}");
    let (title, mark) = &removed[0];
    assert_eq!(title, "Stale", "the row says what was removed");
    assert_eq!(mark.props.get("kind"), Some(&Value::text("removed")));
    assert_eq!(principal(mark, "by").as_deref(), Some(AGENT));
}

#[test]
fn a_field_the_run_changed_takes_its_colour_and_says_what_it_held() {
    let Tidied { ada, groceries, .. } = a_run_in_notes_and_tasks();
    let mut editor = ada.host.open_resource(&groceries, None).unwrap();
    let changed: Vec<UiNode> = all(editor.tree(), "ui.TextInput")
        .into_iter()
        .filter(|input| principal(input, "changed_by").is_some())
        .cloned()
        .collect();
    // The title it renamed, not the body it left alone.
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert_eq!(principal(&changed[0], "changed_by").as_deref(), Some(AGENT));
    assert_eq!(
        changed[0].props.get("previous"),
        Some(&Value::text("Groceries"))
    );
    // The window of the note counts its one change.
    assert!(
        all(editor.tree(), "ui.ChangeMark")
            .iter()
            .any(|m| m.props.get("count") == Some(&Value::Int(1))),
        "the note's window is marked"
    );
    // Once seen, the field is the field again.
    assert!(ada.host.runtime().mark_seen("ada", &groceries).unwrap());
    editor.poll_patches().unwrap();
    assert!(
        all(editor.tree(), "ui.TextInput")
            .iter()
            .all(|input| principal(input, "changed_by").is_none())
    );
}

const PLACED: &str = r#"app so.tilo.lang.placed "Placed"

type Book {
    title: Text
}

type Shelf {
    books: Collection<Book>
}

root Shelf

default view shelf for Shelf

action shelve(s: Shelf, title: Text) -> Book {
    s.books.insert(Book(title: title))
}

/// Each row places the mark itself, before the title.
view shelf(s: Shelf) {
    ui.VirtualList {
        for b in s.books {
            ui.ListItem(item: b) {
                row {
                    ui.ChangeMark(b)
                    text(b.title)
                }
            }
        }
    }
}
"#;

#[test]
fn a_view_that_places_the_mark_itself_gets_it_filled_and_the_host_places_none() {
    let app = "so.tilo.lang.placed";
    let (mut ada, _) = Replica::new("ada", AutoCommit::new(), &[(PLACED, "")]);
    let shelf = Value::resource(&ada.host.root_of(app).unwrap());
    ada.commit();
    call(&ada, AGENT, app, "shelve", vec![shelf, Value::text("Dune")]);
    ada.commit_in_run(AGENT, RUN, "Stock the shelf");
    let mut view = ada.view(app);
    let rows = rows(&mut view);
    let placed: Vec<&UiNode> = rows
        .iter()
        .flat_map(|row| all(row, "ui.ChangeMark"))
        .collect();
    assert_eq!(placed.len(), 1, "one mark, the view's: {rows:?}");
    assert_eq!(principal(placed[0], "by").as_deref(), Some(AGENT));
    assert_eq!(
        placed[0].props.get("run"),
        Some(&Value::text("Stock the shelf"))
    );
}

/// The window's mark of `view`: the header's, not a row's.
fn window_mark(view: &mut ViewInstance) -> UiNode {
    view.poll_patches().unwrap();
    all(view.tree(), "ui.ChangeMark")
        .into_iter()
        .find(|mark| mark.props.get("window") == Some(&Value::Bool(true)))
        .cloned()
        .expect("the window's header is marked")
}

/// What a window's mark opens: the views of its catch-up.
fn catch_up(mark: &UiNode) -> UiNode {
    let slot = mark
        .children
        .iter()
        .find(|child| {
            child.kind == "slot" && child.props.get("name") == Some(&Value::text("content"))
        })
        .unwrap_or_else(|| panic!("the window's mark opens a catch-up: {mark:?}"));
    slot.children
        .first()
        .cloned()
        .expect("the catch-up has views")
}

/// The text a node says, at the font role `role`, anywhere under it.
fn texts(node: &UiNode, role: &str) -> Vec<String> {
    all(node, "ui.Text")
        .into_iter()
        .filter(|text| text.props.get("role") == Some(&Value::text(role)))
        .filter_map(|text| match text.props.get("value") {
            Some(Value::Text(value)) => Some(value.clone()),
            _ => None,
        })
        .collect()
}

/// The buttons anywhere under `node`, by the label they carry.
fn buttons(node: &UiNode) -> Vec<UiNode> {
    all(node, "ui.Button").into_iter().cloned().collect()
}

fn labelled(node: &UiNode, label: &str) -> Option<UiNode> {
    buttons(node)
        .into_iter()
        .find(|button| button.props.get("label") == Some(&Value::text(label)))
}

const BO: &str = "bo";
const SCOUT: &str = "agent:scout";

/// Ada's week when she was away a day: she wrote four tasks, and while she was out Bo took the
/// mail and her agent moved the survey to today and finished the milk.
struct Away {
    ada: Replica,
    paper: ResourceRef,
    survey: ResourceRef,
}

fn a_day_away() -> Away {
    let (mut ada, tasks) = crate::tasks::workspace();
    let make = |ada: &Replica, title: &str| -> ResourceRef {
        match call(
            ada,
            "ada",
            crate::legibility::TASKS_APP,
            "new_task",
            crate::tasks::create(&tasks, title),
        ) {
            Value::Ref(at) => (*at.target).clone(),
            other => panic!("a task: {other:?}"),
        }
    };
    let paper = make(&ada, "Sort the paper mail");
    let _permit = make(&ada, "Ask the neighbours about the parking permit");
    let survey = make(&ada, "Read the survey responses");
    let milk = make(&ada, "Buy oat milk and eggs");
    ada.commit();
    // While she was out: Bo took the mail, their agent brought the survey's day forward and
    // finished the milk.
    call(
        &ada,
        BO,
        crate::legibility::TASKS_APP,
        "assign",
        vec![
            Value::resource(&paper),
            Value::some(Value::Principal(Principal::new("ada"))),
        ],
    );
    ada.commit_as(BO);
    call(
        &ada,
        SCOUT,
        crate::legibility::TASKS_APP,
        "schedule",
        vec![
            Value::resource(&survey),
            Value::some(Value::text("2026-10-29")),
        ],
    );
    call(
        &ada,
        SCOUT,
        crate::legibility::TASKS_APP,
        "check",
        vec![Value::resource(&milk)],
    );
    ada.commit_as(SCOUT);
    Away { ada, paper, survey }
}

/// Ada's day away, opened at Today, where the mail she was given and the survey her agent moved
/// stand, with the mark the window's header shows.
fn a_day_away_at_today() -> (Away, ViewInstance, UiNode) {
    let away = a_day_away();
    let mut tasks = away.ada.view(crate::legibility::TASKS_APP);
    crate::tasks::go_to(&mut tasks, "Today");
    let window = window_mark(&mut tasks);
    (away, tasks, window)
}

/// A day away at a glance: the header names everyone who changed anything and says how much.
#[test]
fn a_day_away_names_in_its_header_who_changed_what() {
    let (_away, _tasks, window) = a_day_away_at_today();
    let mut who = principals(&window);
    who.sort();
    assert_eq!(
        who,
        {
            let mut expected = vec![BO.to_owned(), SCOUT.to_owned()];
            expected.sort();
            expected
        },
        "the header names Bo and Scout"
    );
    assert_eq!(
        window.props.get("count"),
        Some(&Value::Int(3)),
        "three changes fill the header: {window:?}"
    );
}

/// Each marked row says what happened to it, in its own words.
#[test]
fn a_day_away_says_on_each_row_what_was_done_to_it() {
    let (Away { paper, survey, .. }, mut tasks, _window) = a_day_away_at_today();
    let marks = row_marks(&mut tasks);
    let paper = marked(&marks, &paper.id_string())
        .unwrap_or_else(|| panic!("the mail Ada was given is marked: {marks:?}"));
    let survey = marked(&marks, &survey.id_string())
        .unwrap_or_else(|| panic!("the survey her agent moved is marked: {marks:?}"));
    assert_eq!(
        said(paper),
        "assigned it to you",
        "the mail says what was done to it, and who holds it now"
    );
    assert_eq!(
        said(survey),
        "set the due date to 2026-10-29",
        "the survey says the day its agent gave it: {:?}",
        said(survey)
    );
    assert_ne!(said(paper), said(survey), "each row says its own change");
}

/// What the header opens: every step, grouped by the item, newest first, each with who made it
/// and when.
#[test]
fn a_day_away_opens_a_catch_up_of_a_step_per_change() {
    let (_away, _tasks, window) = a_day_away_at_today();
    let content = catch_up(&window);
    assert!(
        texts(&content, "heading")
            .iter()
            .any(|line| line == "Since you looked"),
        "the catch-up says what it is: {content:?}"
    );
    let groups: Vec<String> = buttons(&content)
        .into_iter()
        .filter_map(|button| match button.props.get("label") {
            Some(Value::Text(label)) => Some(label.clone()),
            _ => None,
        })
        .filter(|label| label != "Mark all seen")
        .collect();
    assert_eq!(
        groups,
        [
            "Buy oat milk and eggs",
            "Read the survey responses",
            "Sort the paper mail"
        ],
        "the catch-up names each item it changed, newest first"
    );
    let seen_by: Vec<String> = all(&content, "ui.Avatar")
        .into_iter()
        .filter_map(|avatar| principal(avatar, "who"))
        .collect();
    assert!(seen_by.contains(&SCOUT.to_owned()), "{seen_by:?}");
    assert!(seen_by.contains(&BO.to_owned()), "{seen_by:?}");
    assert!(
        !all(&content, "ui.Time").is_empty(),
        "each step says when it happened: {content:?}"
    );
}

/// Each step reads as a sentence, then when it happened at the line's end, out of its way,
/// led by the changer's face.
#[test]
fn a_day_aways_catch_up_puts_the_time_at_the_lines_end() {
    let (_away, _tasks, window) = a_day_away_at_today();
    let content = catch_up(&window);
    let mut steps = Vec::new();
    find_all(&content, "row", &mut steps);
    steps.retain(|step| {
        step.children
            .first()
            .is_some_and(|child| child.kind == "ui.Avatar")
    });
    assert_eq!(
        steps.len(),
        3,
        "the catch-up lists a step per change: {content:?}"
    );
    for step in &steps {
        let kids = &step.children;
        let said = kids
            .iter()
            .position(|child| child.kind == "ui.Text")
            .unwrap_or_else(|| panic!("a step says what was done: {step:?}"));
        let at = kids
            .iter()
            .position(|child| child.kind == "ui.Time")
            .unwrap_or_else(|| panic!("a step says when: {step:?}"));
        assert!(said < at, "what was done reads before when: {step:?}");
        assert_eq!(
            at,
            kids.len() - 1,
            "the time stands at the line's end: {step:?}"
        );
        assert_eq!(
            kids.get(at - 1).map(|child| child.kind.as_str()),
            Some("spacer"),
            "the sentence is kept clear of the time: {step:?}"
        );
        assert_eq!(
            kids.first().map(|child| child.kind.as_str()),
            Some("ui.Avatar"),
            "the changer's face leads the line: {step:?}"
        );
    }
}

/// What it says is history's words: a flag marked, a person given to the reader, and the item
/// itself as "it", never "this".
#[test]
fn a_day_aways_catch_up_says_each_step_in_historys_words() {
    let (_away, _tasks, window) = a_day_away_at_today();
    let content = catch_up(&window);
    let lines: Vec<String> = all(&content, "ui.Text")
        .into_iter()
        .filter_map(|text| match text.props.get("value") {
            Some(Value::Text(value)) => Some(value.clone()),
            _ => None,
        })
        .collect();
    let joined = lines.join(" | ");
    assert!(joined.contains("marked it done"), "{joined}");
    assert!(joined.contains("assigned it to you"), "{joined}");
    assert!(
        !joined.contains("this"),
        "no sentence says \"this\": {joined}"
    );
    assert!(
        !joined.contains("done to"),
        "a flag shows no value: {joined}"
    );
}

/// Opening it marks nothing: only the footer's one action does, and its undo gives the marks
/// back.
#[test]
fn a_day_aways_catch_up_marks_all_seen_at_once_and_undo_brings_them_back() {
    let (away, mut tasks, window) = a_day_away_at_today();
    let content = catch_up(&window);
    assert!(
        away.ada.host.runtime().take_seen().is_empty(),
        "opening the catch-up marks nothing"
    );
    let footer = labelled(&content, "Mark all seen").expect("the catch-up's footer");
    tasks.fire(&footer.key, "press").unwrap();
    tasks.poll_patches().unwrap();
    let wrote = away.ada.host.runtime().take_seen();
    assert_eq!(wrote.len(), 3, "one action marks all three seen: {wrote:?}");
    assert!(
        row_marks(&mut tasks).iter().all(|(_, mark)| mark.is_none()),
        "every mark goes at once"
    );
    assert!(
        away.ada.host.runtime().undo_seen("ada"),
        "the catch-up is undone"
    );
    tasks.poll_patches().unwrap();
    assert!(
        row_marks(&mut tasks).iter().any(|(_, mark)| mark.is_some()),
        "undo brings the marks back"
    );
}

/// The header's mark reads what changed, not the workspace: at ten thousand items, three changes
/// cost what three changes cost at a thousand.
#[test]
fn the_headers_mark_reads_the_changes_not_the_items() {
    fn away(n: usize) -> (ViewInstance, usize) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/tasks");
        let compiled = tilolang_tilo::bench::compile(&dir).expect("the tasks package compiles");
        let doc = tilolang_tilo::bench::seeded_record_by(&compiled, n, "ada")
            .expect("the workspace seeds");
        let (mut ada, _) = Replica::new("ada", doc, crate::tasks::PACKAGES);
        let root = ada.host.root_of(crate::legibility::TASKS_APP).unwrap();
        let items = match ada.host.runtime().read(&root, "items").unwrap() {
            Value::List(items) => items,
            other => panic!("the tasks are a list: {other:?}"),
        };
        for item in items.iter().take(3) {
            call(
                &ada,
                BO,
                crate::legibility::TASKS_APP,
                "check",
                vec![item.clone()],
            );
        }
        ada.commit_as(BO);
        let view = ada.view(crate::legibility::TASKS_APP);
        (view, items.len())
    }
    let (small, small_items) = away(1_000);
    let (large, large_items) = away(10_000);
    assert_eq!(small_items, 1_000);
    assert_eq!(large_items, 10_000);
    let (small, large) = (small.stats(), large.stats());
    assert_eq!(
        small.reads, large.reads,
        "the header's reads at ten thousand items ({large:?}) are the reads at a thousand ({small:?})"
    );
}
