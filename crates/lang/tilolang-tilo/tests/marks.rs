//! Marks in place (task-legibility.md §3): with no code of the app's, the host marks the rows,
//! the bound fields and the window of what an agent's run changed and the person has not seen;
//! each mark shows the run that made it; a view may place the mark itself; and every mark goes
//! once the run is seen.

use crate::fixtures::Workspace;
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewEvent, ViewInstance};

use crate::fixtures::Replica;
use crate::fixtures::call;
use crate::legibility::{AGENT, NOTES_APP, Tidied, an_agent_tidies_notes_and_tasks, helper};

/// Every row of the view's list, as the list builds them.
fn rows(view: &mut ViewInstance) -> Vec<UiNode> {
    view.poll_patches().unwrap();
    let list = view
        .tree()
        .of_kind("ui.VirtualList")
        .first()
        .expect("a list")
        .key
        .clone();
    let len = view.list_len(&list).unwrap_or(0);
    (0..len)
        .filter_map(|i| view.row(&list, i).unwrap().cloned())
        .collect()
}

/// The resource each row presents, and the change mark placed on it, if any.
fn row_marks(view: &mut ViewInstance) -> Vec<(String, Option<UiNode>)> {
    let mut out = Vec::new();
    for row in rows(view) {
        for item in row.of_kind("ui.ListItem") {
            let of = match item.props.get("item") {
                Some(Value::Ref(r)) => r.target.id_string(),
                other => panic!("a row presents its item: {other:?}"),
            };
            let mark = item
                .children
                .iter()
                .find(|c| c.kind == "part" && c.props.get("name") == Some(&Value::text("change")))
                .and_then(|part| part.children.first().cloned());
            out.push((of, mark));
        }
    }
    out
}

/// The resource each row of the view's tree presents, and the change mark placed on it, if any.
fn tree_marks(view: &mut ViewInstance) -> Vec<(String, Option<UiNode>)> {
    view.set_lazy_rows(false).unwrap();
    view.poll_patches().unwrap();
    view.tree()
        .of_kind("ui.TreeViewNode")
        .into_iter()
        .map(|node| {
            let of = match node.props.get("item") {
                Some(Value::Ref(r)) => r.target.id_string(),
                other => panic!("a row presents its item: {other:?}"),
            };
            let mark = node
                .children
                .iter()
                .find(|c| c.kind == "part" && c.props.get("name") == Some(&Value::text("change")))
                .and_then(|part| part.children.first().cloned());
            (of, mark)
        })
        .collect()
}

fn actor(node: &UiNode, prop: &str) -> Option<String> {
    match node.props.get(prop) {
        Some(Value::Actor(who)) => Some(who.id().to_owned()),
        _ => None,
    }
}

/// Everyone the mark names, in the order it names them.
fn actors(node: &UiNode) -> Vec<String> {
    match node.props.get("by_all") {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::Actor(who) => Some(who.id().to_owned()),
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

tilo_test_env::scenario! {
    fn the_host_marks_what_a_run_changed;

    phase the_host_marks_rows_and_the_window_and_a_mark_shows_its_run;
    {
        let tidied = an_agent_tidies_notes_and_tasks();
        let Tidied {
            ada,
            made,
            groceries,
            ..
        } = &tidied;

        // The notebook: the rows the run made or changed carry its mark, with who and which run;
        // no app code asked for it.
        let mut notebook = ada.view(NOTES_APP);
        let marks = tree_marks(&mut notebook);
        for (at, kind) in [(&made, "created"), (&groceries, "changed")] {
            let mark = marked(&marks, &at.id_string())
                .unwrap_or_else(|| panic!("{at:?} is marked: {marks:?}"));
            assert_eq!(mark.kind, "ui.ChangeMark");
            assert_eq!(actor(mark, "by").as_deref(), Some(AGENT));
            assert_eq!(mark.props.get("kind"), Some(&Value::text(kind)));
        }
        // What each marked row says under its title: the change in words.
        assert_eq!(
            said(marked(&marks, &made.id_string()).unwrap()),
            "created it"
        );
        assert_eq!(
            said(marked(&marks, &groceries.id_string()).unwrap()),
            "renamed",
            "a write of the field a note is named by renames it"
        );
        // The window's header counts what changed in it, and by whom.
        let window = notebook.tree().of_kind("ui.ChangeMark")
            .into_iter()
            .find(|m| m.props.get("window") == Some(&Value::Bool(true)))
            .cloned()
            .expect("the window's header is marked");
        assert_eq!(actor(&window, "by").as_deref(), Some(AGENT));
        assert_eq!(
            actors(&window),
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
                ViewEvent::Review { target, .. } if target.id_string() == groceries.id_string()
            )),
            "the mark opens what it marked"
        );

        // Mark all seen: every mark goes, live.
        crate::legibility::mark_all_seen(&tidied);
        let marks = tree_marks(&mut notebook);
        assert!(marks.iter().all(|(_, mark)| mark.is_none()), "{marks:?}");
        assert!(
            notebook.tree().of_kind("ui.ChangeMark").is_empty(),
            "the window's mark is gone"
        );
    }

    phase a_field_the_run_changed_takes_its_colour_and_says_what_it_held;
    {
        let Tidied { ada, groceries, .. } = an_agent_tidies_notes_and_tasks();
        let mut editor = ada.host.open_resource(&groceries, None).unwrap();
        let changed: Vec<UiNode> = editor.tree().of_kind("ui.TextInput")
            .into_iter()
            .filter(|input| actor(input, "changed_by").is_some())
            .cloned()
            .collect();
        // The title it renamed, not the body it left alone.
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(actor(&changed[0], "changed_by").as_deref(), Some(AGENT));
        assert_eq!(
            changed[0].props.get("previous"),
            Some(&Value::text("Groceries"))
        );
        // The window of the note counts its one change.
        assert!(
            editor.tree().of_kind("ui.ChangeMark")
                .iter()
                .any(|m| m.props.get("count") == Some(&Value::Int(1))),
            "the note's window is marked"
        );
        // Once seen, the field is the field again.
        assert!(ada.host.runtime().mark_seen(&tilolang_tilo::personal::actor("ada"), &groceries).unwrap());
        editor.poll_patches().unwrap();
        assert!(
            editor.tree().of_kind("ui.TextInput")
                .iter()
                .all(|input| actor(input, "changed_by").is_none())
        );
    }

    phase a_view_that_places_the_mark_itself_gets_it_filled_and_the_host_places_none;
    {
        let app = "so.tilo.lang.placed";
        let (ada, _) = Replica::new("ada", Workspace::new(), &[(PLACED, "")]);
        let shelf = Value::resource(&ada.host.root_of(app).unwrap());
        ada.commit();
        crate::legibility::in_run(&ada, "Stock the shelf");
        call(&ada, &helper(), app, "shelve", vec![shelf, Value::text("Dune")]);
        ada.host.store().set_run(None);
        ada.commit();
        let mut view = ada.view(app);
        let rows = rows(&mut view);
        let placed: Vec<&UiNode> = rows
            .iter()
            .flat_map(|row| row.of_kind("ui.ChangeMark"))
            .collect();
        assert_eq!(placed.len(), 1, "one mark, the view's: {rows:?}");
        assert_eq!(actor(placed[0], "by").as_deref(), Some(AGENT));
        assert_eq!(
            placed[0].props.get("run"),
            Some(&Value::text("Stock the shelf"))
        );
    }
}

/// The window's mark names everyone who changed what it shows, newest first: not only whoever
/// happened to change it last.
#[test]
fn the_header_names_everyone_who_changed_it() {
    const BO: &str = "bo";
    let Tidied { ada, .. } = an_agent_tidies_notes_and_tasks();
    let notes = Value::resource(&ada.host.root_of(NOTES_APP).unwrap());
    call(&ada, BO, NOTES_APP, "new_note", vec![notes]);

    let mut notebook = ada.view(NOTES_APP);
    notebook.poll_patches().unwrap();
    let window = notebook
        .tree()
        .of_kind("ui.ChangeMark")
        .into_iter()
        .find(|m| m.props.get("window") == Some(&Value::Bool(true)))
        .cloned()
        .expect("the window's header is marked");
    let mut who = actors(&window);
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

/// A change the person watched land, its live mark on screen, is seen: its row and the window's
/// header no longer tell them of it. A change someone else made while they were not looking
/// keeps its marks.
#[test]
fn a_change_watched_landing_is_seen_and_one_made_unwatched_is_not() {
    let Tidied {
        ada,
        groceries,
        landlord,
        ..
    } = an_agent_tidies_notes_and_tasks();
    let tasks = crate::legibility::TASKS_APP;
    let (me, bo) = (
        tilolang_tilo::personal::actor("ada"),
        tilolang_tilo::personal::actor("bo"),
    );
    // Bo's earlier change of the task, unwatched, stands under the agent's.
    call(
        &ada,
        &bo,
        tasks,
        "check",
        vec![Value::resource(&landlord), Value::Bool(false)],
    );
    ada.commit();
    call(
        &ada,
        &helper(),
        tasks,
        "check",
        vec![Value::resource(&landlord), Value::Bool(true)],
    );
    ada.commit();

    {
        let runtime = ada.host.runtime();
        // Who the live mark names matters: Bo did not rename the note.
        assert!(!runtime.seen_live(&me, &groceries, &bo).unwrap());
        assert!(runtime.seen_live(&me, &groceries, &helper()).unwrap());
        // Bo's change, made while Ada was not looking, keeps the task marked: the agent's first, Bo's
        // and the one she watched.
        assert!(!runtime.seen_live(&me, &landlord, &helper()).unwrap());
        assert_eq!(runtime.unseen(&me, &landlord).unwrap().len(), 3);
    }
    let marks = tree_marks(&mut ada.view(NOTES_APP));
    assert!(
        marked(&marks, &groceries.id_string()).is_none(),
        "the row says nothing of a change the person watched land"
    );
}

/// The rows the host placed in `tree`'s lists and trees for removed items: each one's title and
/// mark.
fn removed_rows(tree: &UiNode) -> Vec<(String, UiNode)> {
    let mut out = Vec::new();
    for holder in tree
        .of_kind("ui.VirtualList")
        .into_iter()
        .chain(tree.of_kind("ui.TreeView"))
    {
        let Some(part) = holder
            .children
            .iter()
            .find(|c| c.kind == "part" && c.props.get("name") == Some(&Value::text("removed")))
        else {
            continue;
        };
        for item in &part.children {
            assert_eq!(item.props.get("removed"), Some(&Value::Bool(true)));
            // A list's row is a list row under it; a tree's node says what it was called.
            let title = item
                .of_kind("ui.ListRow")
                .first()
                .and_then(|row| row.props.get("title").cloned())
                .or_else(|| item.props.get("label").cloned());
            let mark = item.of_kind("ui.ChangeMark").first().map(|m| (*m).clone());
            if let (Some(Value::Text(title)), Some(mark)) = (title, mark) {
                out.push((title, mark));
            }
        }
    }
    out
}

#[test]
fn a_task_the_run_removed_stays_a_struck_row_until_its_removal_is_seen() {
    let tidied = an_agent_tidies_notes_and_tasks();
    let ada = &tidied.ada;
    let mut inbox = ada.view(crate::legibility::TASKS_APP);
    crate::tasks::go_to(&mut inbox, "Inbox");
    inbox.poll_patches().unwrap();
    let removed = removed_rows(inbox.tree());
    assert_eq!(removed.len(), 1, "{removed:?}");
    let (title, mark) = &removed[0];
    assert_eq!(title, "Stale", "the row says what was removed");
    assert_eq!(mark.props.get("kind"), Some(&Value::text("removed")));
    assert_eq!(actor(mark, "by").as_deref(), Some(AGENT));
    // The notebook's tree stands the note it removed in too, as a node of its own
    // (`a_note_the_run_removed_stands_struck_in_the_tree`).
    let mut notebook = ada.view(NOTES_APP);
    notebook.poll_patches().unwrap();
    let note_rows = removed_rows(notebook.tree());
    assert_eq!(note_rows.len(), 1, "{note_rows:?}");
    // Seen, the row goes.
    crate::legibility::mark_all_seen(&tidied);
    inbox.poll_patches().unwrap();
    assert!(removed_rows(inbox.tree()).is_empty());
    notebook.poll_patches().unwrap();
    assert!(removed_rows(notebook.tree()).is_empty());
}

/// A list built over a document that already holds the removal shows the row struck too: the
/// index knows what left each collection without reading the log.
#[test]
fn a_list_built_over_the_document_shows_what_left_it() {
    let Tidied { ada, .. } = an_agent_tidies_notes_and_tasks();
    let fresh = crate::legibility::built_over(&ada);
    let mut inbox = fresh.open_main(crate::legibility::TASKS_APP).unwrap();
    crate::tasks::go_to(&mut inbox, "Inbox");
    inbox.poll_patches().unwrap();
    let removed = removed_rows(inbox.tree());
    assert_eq!(removed.len(), 1, "{removed:?}");
    let (title, mark) = &removed[0];
    assert_eq!(title, "Stale", "the row says what was removed");
    assert_eq!(mark.props.get("kind"), Some(&Value::text("removed")));
    assert_eq!(actor(mark, "by").as_deref(), Some(AGENT));
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

/// The window's mark of `view`: the header's, not a row's.
fn window_mark(view: &mut ViewInstance) -> UiNode {
    view.poll_patches().unwrap();
    view.tree()
        .of_kind("ui.ChangeMark")
        .into_iter()
        .find(|mark| mark.props.get("window") == Some(&Value::Bool(true)))
        .cloned()
        .expect("the window's header is marked")
}

/// What a window's mark opens: the views of its catch-up.
fn catch_up(mark: &UiNode) -> UiNode {
    let part = mark
        .children
        .iter()
        .find(|child| {
            child.kind == "part" && child.props.get("name") == Some(&Value::text("content"))
        })
        .unwrap_or_else(|| panic!("the window's mark opens a catch-up: {mark:?}"));
    part.children
        .first()
        .cloned()
        .expect("the catch-up has views")
}

/// The text a node says, at the font role `role`, anywhere under it.
fn texts(node: &UiNode, role: &str) -> Vec<String> {
    node.of_kind("ui.Text")
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
    node.of_kind("ui.Button").into_iter().cloned().collect()
}

fn labelled(node: &UiNode, label: &str) -> Option<UiNode> {
    buttons(node)
        .into_iter()
        .find(|button| button.props.get("label") == Some(&Value::text(label)))
}

const BO: &str = "bo";
const SCOUT: &str = "scout";

fn scout() -> Actor {
    tilolang_tilo::runtime::ids::MemberId::agent(SCOUT, "ada").into()
}

/// Ada's week when she was away a day: she wrote four tasks, and while she was out Bo took the
/// mail and her agent moved the survey to today and finished the milk.
struct Away {
    ada: Replica,
    paper: ResourceRef,
    survey: ResourceRef,
}

fn a_day_away() -> Away {
    let (ada, tasks) = crate::tasks::workspace();
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
            Value::some(Value::Actor(Actor::from(
                tilolang_tilo::runtime::ids::MemberId::person("ada"),
            ))),
        ],
    );
    call(
        &ada,
        &scout(),
        crate::legibility::TASKS_APP,
        "schedule",
        vec![
            Value::resource(&survey),
            Value::some(Value::text("2026-10-29")),
        ],
    );
    call(
        &ada,
        &scout(),
        crate::legibility::TASKS_APP,
        "check",
        vec![Value::resource(&milk)],
    );
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

tilo_test_env::scenario! {
    fn a_day_away_reads_as_a_catch_up;

    phase a_day_away_names_in_its_header_who_changed_what;
    // A day away at a glance: the header names everyone who changed anything and says how much.
    {
        let (_away, _tasks, window) = a_day_away_at_today();
        let mut who = actors(&window);
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

    phase a_day_away_says_on_each_row_what_was_done_to_it;
    // Each marked row says what happened to it, in its own words.
    {
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

    phase a_day_away_opens_a_catch_up_of_a_row_per_item;
    // What the header opens: a row per item someone changed, newest first, each with who made the
    // newest change of it and when.
    {
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
            "the catch-up names each item it changed, newest first, once"
        );
        let seen_by: Vec<String> = content.of_kind("ui.Avatar")
            .into_iter()
            .filter_map(|avatar| actor(avatar, "who"))
            .collect();
        assert!(seen_by.contains(&SCOUT.to_owned()), "{seen_by:?}");
        assert!(seen_by.contains(&BO.to_owned()), "{seen_by:?}");
        assert!(
            !content.of_kind("ui.Time").is_empty(),
            "each step says when it happened: {content:?}"
        );
    }

    phase a_day_aways_catch_up_puts_the_time_at_the_lines_end;
    // Each row's line reads as a sentence, then when it happened at the line's end, out of its
    // way, led by the changers' faces.
    {
        let (_away, _tasks, window) = a_day_away_at_today();
        let content = catch_up(&window);
        let mut steps = content.of_kind("row");
        steps.retain(|step| {
            step.children
                .last()
                .is_some_and(|child| child.kind == "ui.Time")
        });
        assert_eq!(
            steps.len(),
            3,
            "the catch-up lists one line per item changed: {content:?}"
        );
        for step in &steps {
            let kids = &step.children;
            let at = kids
                .iter()
                .position(|child| child.kind == "ui.Time")
                .unwrap_or_else(|| panic!("a step says when: {step:?}"));
            assert_eq!(
                at,
                kids.len() - 1,
                "the time stands at the line's end: {step:?}"
            );
            // What they did is the line's own group, which takes the room the time leaves: it wraps
            // rather than squeezing the faces it opens with.
            let told = kids
                .first()
                .filter(|child| child.kind == "row")
                .unwrap_or_else(|| panic!("the sentence is its own group: {step:?}"));
            assert_eq!(
                told.props.get("wrap"),
                Some(&Value::Bool(true)),
                "the sentence wraps rather than squeezing what it holds: {told:?}"
            );
            assert_eq!(
                told.children.first().map(|child| child.kind.as_str()),
                Some("ui.Avatar"),
                "the changers' faces lead the line: {step:?}"
            );
            let said = told
                .children
                .iter()
                .position(|child| child.kind == "ui.Text")
                .unwrap_or_else(|| panic!("a step says what was done: {step:?}"));
            assert!(
                said > 0 && said < told.children.len() - 1,
                "the names it opens with stand before what they did: {step:?}"
            );
        }
    }

    phase a_document_changed_many_times_stands_as_one_row;
    // A document someone changed many times since the person looked stands as one row: it says who
    // changed it and what the newest change did, and, as history folds the changes of one hand,
    // how many it folds. Never a line per change.
    {
        let (ada, _) = a_task_bo_changed(4);
        let window = window_mark(&mut ada.view(crate::legibility::TASKS_APP));
        assert_eq!(
            window.props.get("count"),
            Some(&Value::Int(1)),
            "one document changed: {window:?}"
        );
        let content = catch_up(&window);
        let mut lines = content.of_kind("row");
        lines.retain(|line| {
            line.children
                .first()
                .is_some_and(|child| child.kind == "ui.Avatar")
        });
        assert_eq!(
            lines.len(),
            1,
            "one line for the document, however many changes it holds: {content:?}"
        );
        assert_eq!(
            texts(&content, "caption"),
            ["4 changes"],
            "the row says how many changes it folds: {content:?}"
        );
    }
}

/// What it says is history's words: the app's own words for what it did, a person given to the
/// reader, and the item itself as "it", never "this".
#[test]
fn a_day_aways_catch_up_says_each_step_in_historys_words() {
    let (_away, _tasks, window) = a_day_away_at_today();
    let content = catch_up(&window);
    let lines: Vec<String> = content
        .of_kind("ui.Text")
        .into_iter()
        .filter_map(|text| match text.props.get("value") {
            Some(Value::Text(value)) => Some(value.clone()),
            _ => None,
        })
        .collect();
    let joined = lines.join(" | ");
    // Checking a task off moves it with the status action, whose own words history says: the
    // status it went to, once. The `done` flag the status keeps in step for a row's box is
    // bookkeeping the type hides, so history never says it beside the status.
    assert!(joined.contains("scout | set status to Done"), "{joined}");
    assert!(!joined.contains("marked it done"), "{joined}");
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

/// Ada's milk, seen when she made it and changed `changes` times by Bo since.
fn a_task_bo_changed(changes: usize) -> (Replica, ResourceRef) {
    let (ada, tasks) = crate::tasks::workspace();
    ada.host.set_clock(|| "2026-10-30T09:00:00Z".to_owned());
    let milk = match call(
        &ada,
        "ada",
        crate::legibility::TASKS_APP,
        "new_task",
        crate::tasks::create(&tasks, "Buy oat milk and eggs"),
    ) {
        Value::Ref(at) => (*at.target).clone(),
        other => panic!("a task: {other:?}"),
    };
    ada.commit();
    assert!(
        ada.host
            .runtime()
            .mark_seen(&tilolang_tilo::personal::actor("ada"), &milk)
            .unwrap(),
        "she has seen the task she made"
    );
    // Bo at their window a moment later, changing it again and again.
    ada.host.set_clock(|| "2026-10-30T09:10:00Z".to_owned());
    for _ in 0..changes {
        call(
            &ada,
            BO,
            crate::legibility::TASKS_APP,
            "check",
            vec![Value::resource(&milk)],
        );
    }
    (ada, milk)
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
        away.ada
            .host
            .runtime()
            .undo_seen(&tilolang_tilo::personal::actor("ada")),
        "the catch-up is undone"
    );
    tasks.poll_patches().unwrap();
    assert!(
        row_marks(&mut tasks).iter().any(|(_, mark)| mark.is_some()),
        "undo brings the marks back"
    );
}

/// A note the run removed stands struck in the notebook's tree until the person has seen it go:
/// a node of the tree, after its rows, with who removed it.
#[test]
fn a_note_the_run_removed_stands_struck_in_the_tree() {
    let tidied = an_agent_tidies_notes_and_tasks();
    let Tidied { ada, old, .. } = &tidied;
    let mut notebook = ada.view(NOTES_APP);
    notebook.poll_patches().unwrap();
    let struck = |view: &mut ViewInstance| -> Vec<UiNode> {
        view.poll_patches().unwrap();
        view.tree()
            .of_kind("ui.TreeView")
            .into_iter()
            .flat_map(|tree| tree.children.iter())
            .filter(|child| {
                child.kind == "part" && child.props.get("name") == Some(&Value::text("removed"))
            })
            .flat_map(|part| part.children.iter().cloned())
            .collect()
    };
    let rows = struck(&mut notebook);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let node = &rows[0];
    assert_eq!(
        node.kind, "ui.TreeViewNode",
        "it stands as a row of the tree"
    );
    assert_eq!(node.props.get("removed"), Some(&Value::Bool(true)));
    assert!(
        matches!(node.props.get("item"), Some(Value::Ref(r)) if r.target.id_string() == old.id_string()),
        "it says the note it stood for: {node:?}"
    );
    let mark = node
        .of_kind("ui.ChangeMark")
        .into_iter()
        .find(|mark| mark.props.get("window") != Some(&Value::Bool(true)))
        .cloned()
        .expect("who removed it");
    assert_eq!(mark.props.get("kind"), Some(&Value::text("removed")));
    assert_eq!(actor(&mark, "by").as_deref(), Some(AGENT));
    // Seen, the row goes.
    crate::legibility::mark_all_seen(&tidied);
    assert!(struck(&mut notebook).is_empty());
}

/// A note with a field of every kind, and an action that writes each: what history's words are
/// made of.
const WORDS: &str = r#"app so.tilo.words "Words"

type Project {
    name: Text
}

enum Priority {
    none
    high
}

type Note {
    title: Text
    due: Option<Date> = .none
    starts: Option<Time> = .none
    assignee: Option<Actor> = .none
    project: Option<Project> = .none
    priority: Option<Priority> = .none
    pinned: Bool = false
    votes: Int = 0
    ratio: Float = 0.5
    tags: Set<Text>
}

type Notebook {
    notes: Collection<Note>
    projects: Collection<Project>
}

root Notebook

default view notebook for Notebook

view notebook(nb: Notebook) {
    ui.VirtualList {
        for n in nb.notes {
            ui.ListItem(item: n) {
                text(n.title)
            }
        }
    }
}

action new_note(nb: Notebook, title: Text) -> Note {
    nb.notes.insert(Note(title: title))
}

action new_dated(nb: Notebook, title: Text, due: Date) -> Note {
    nb.notes.insert(Note(title: title, due: due))
}

action new_started(nb: Notebook, title: Text, starts: Time) -> Note {
    nb.notes.insert(Note(title: title, starts: starts))
}

action new_assigned(nb: Notebook, title: Text, who: Actor) -> Note {
    nb.notes.insert(Note(title: title, assignee: who))
}

action new_filed(nb: Notebook, title: Text, project: Project) -> Note {
    nb.notes.insert(Note(title: title, project: project))
}

action new_project(nb: Notebook, name: Text) -> Project {
    nb.projects.insert(Project(name: name))
}

action retitle(n: Note, title: Text) {
    n.title = title
}

action schedule(n: Note, due: Option<Date>) {
    n.due = due
}

action start_at(n: Note, starts: Option<Time>) {
    n.starts = starts
}

action assign(n: Note, who: Option<Actor>) {
    n.assignee = who
}

action file(n: Note, project: Option<Project>) {
    n.project = project
}

action new_high(nb: Notebook, title: Text) -> Note {
    nb.notes.insert(Note(title: title, priority: .some(.high)))
}

action new_pinned(nb: Notebook, title: Text) -> Note {
    nb.notes.insert(Note(title: title, pinned: true))
}

action prioritize(n: Note) {
    n.priority = .some(.high)
}

action deprioritize(n: Note) {
    n.priority = .none
}

action pin(n: Note, pinned: Bool) {
    n.pinned = pinned
}

action vote(n: Note, votes: Int) {
    n.votes = votes
}

action weigh(n: Note, ratio: Float) {
    n.ratio = ratio
}

action tag(n: Note, tag: Text) {
    n.tags.add(tag)
}
"#;

tilo_test_env::scenario! {
    fn a_field_of_every_kind_reads_as_what_was_done_to_it;

    phase the_catch_up_says_each_kind_of_change_in_its_own_words;
    // One note per kind of field, each changed by someone else while the person looking was
    // away: the catch-up the window's mark opens says what was done to each, in history's
    // words, with the value the words speak of.
    {
        let words = "so.tilo.words";
        let (ada, _) = Replica::new("ada", Workspace::new(), &[(WORDS, "")]);
        ada.host.set_clock(|| "2026-10-29T10:00:00Z".to_owned());
        let root = Value::resource(&ada.host.root_of(words).unwrap());
        ada.commit();
        let make = |action: &str, args: &[Value]| -> ResourceRef {
            let mut all = vec![root.clone()];
            all.extend_from_slice(args);
            match call(&ada, "ada", words, action, all) {
                Value::Ref(at) => (*at.target).clone(),
                other => panic!("a note: {other:?}"),
            }
        };
        let project = make("new_project", &[Value::text("Work")]);
        let renamed = make("new_note", &[Value::text("Rename me")]);
        let moved = make("new_dated", &[Value::text("Move me"), Value::text("2026-10-27")]);
        let cleared = make("new_dated", &[Value::text("Clear my day"), Value::text("2026-10-27")]);
        let timed = make("new_note", &[Value::text("Give me a time")]);
        let untimed = make("new_started", &[Value::text("Take my time away"), Value::text("2026-10-29T09:00:00Z")]);
        let handed = make(
            "new_assigned",
            &[Value::text("Hand me back"), Value::Actor(Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")))],
        );
        let filed = make("new_filed", &[Value::text("File me out"), Value::resource(&project)]);
        let shelved = make("new_note", &[Value::text("Give me a project")]);
        let high_set = make("new_note", &[Value::text("Raise me")]);
        let high_cleared = make("new_high", &[Value::text("Lower me")]);
        let pinned_set = make("new_note", &[Value::text("Pin me")]);
        let pinned_cleared = make("new_pinned", &[Value::text("Unpin me")]);
        let counted = make("new_note", &[Value::text("Count me")]);
        let weighed = make("new_note", &[Value::text("Weigh me")]);
        let tagged = make("new_note", &[Value::text("Tag me")]);
        ada.commit();

        // Bo changes them while Ada is away.
        let note = |at: &ResourceRef| Value::resource(at);
        let bo = "bo";
        call(&ada, bo, words, "retitle", vec![note(&renamed), Value::text("Renamed")]);
        call(&ada, bo, words, "schedule", vec![note(&moved), Value::some(Value::text("2026-10-30"))]);
        call(&ada, bo, words, "schedule", vec![note(&cleared), Value::none()]);
        call(&ada, bo, words, "start_at", vec![note(&timed), Value::some(Value::text("2026-10-29T15:00:00Z"))]);
        call(&ada, bo, words, "start_at", vec![note(&untimed), Value::none()]);
        call(&ada, bo, words, "assign", vec![note(&handed), Value::none()]);
        call(&ada, bo, words, "file", vec![note(&filed), Value::none()]);
        call(&ada, bo, words, "file", vec![note(&shelved), Value::some(Value::resource(&project))]);
        call(&ada, bo, words, "prioritize", vec![note(&high_set)]);
        call(&ada, bo, words, "deprioritize", vec![note(&high_cleared)]);
        call(&ada, bo, words, "pin", vec![note(&pinned_set), Value::Bool(true)]);
        call(&ada, bo, words, "pin", vec![note(&pinned_cleared), Value::Bool(false)]);
        call(&ada, bo, words, "vote", vec![note(&counted), Value::Int(3)]);
        call(&ada, bo, words, "weigh", vec![note(&weighed), Value::Float(0.75)]);
        call(&ada, bo, words, "tag", vec![note(&tagged), Value::text("home")]);
        ada.commit();

        let mut notebook = ada.view(words);
        let marks = row_marks(&mut notebook);
        let said_of = |at: &ResourceRef| -> String {
            match marked(&marks, &at.id_string()) {
                Some(mark) => said(mark),
                None => panic!("{at:?} is marked: {marks:?}"),
            }
        };
        let got = vec![
            said_of(&renamed),
            said_of(&moved),
            said_of(&cleared),
            said_of(&timed),
            said_of(&untimed),
            said_of(&handed),
            said_of(&filed),
            said_of(&shelved),
            said_of(&high_set),
            said_of(&high_cleared),
            said_of(&pinned_set),
            said_of(&pinned_cleared),
            said_of(&counted),
            said_of(&weighed),
            said_of(&tagged),
        ];
        assert_eq!(
            got,
            [
                "renamed",
                "moved it to 2026-10-30",
                "cleared the due date",
                "set the starts time to 2026-10-29T15:00:00Z",
                "cleared the starts time",
                "unassigned it",
                "moved it out of Work",
                "moved it to Work",
                "set priority to High",
                "cleared priority",
                "marked it pinned",
                "unmarked it pinned",
                "set votes to 3",
                "set ratio to 0.75",
                "changed tags",
            ]
        );
    }
}
