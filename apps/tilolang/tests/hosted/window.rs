//! A hosted window as a person reads and uses it: the notebook's tree and the note beside it, the
//! window's search over a list, a tree and an open note, the chat's channels, what an agent finds
//! and runs at the door, and a send that leaves the workspace only once a person allows it.

use serde_json::{Map, Value as Json, json};
use tilo_ui_host::reconcile::{UiEvent, display};
use tilo_ui_host::{UiNode, Value};
use tilolang_tilo::runtime::{Principal, ResourceRef, Value as RtValue};
use ui::component::RowSource as _;
use ui::component_lifecycle::WindowTitle as _;
use ui::tilo_actions::{ActionContext, ActionInvoker};
use ui::widget::{FilterCount, FindOptions, InPlace, SearchDriver};

use super::{CHAT, NOTES, Session, TASKS, find, note_row, place_at, press_new_note};
use crate::change_marks::action_id;

const HOST: &str = "tl:host";
const LETTERS: &str = "test.letters";

/// An agent's task `7` on behalf of `ada`.
fn task() -> (ActionInvoker, ActionContext) {
    (
        ActionInvoker::agent("agent-task:7").acting_for("ada"),
        ActionContext::default().with_group("task:7", "Tidy the notes", "task"),
    )
}

/// `app`'s window in the frame harness with the scenario `scenario` staged.
fn staged(app: &str, scenario: &str) -> Session {
    let mut session = Session::new(app, "ada");
    session.instance_mut().stage(scenario);
    session.settle();
    session
}

/// The shell the window composed of what its app sent.
fn composed(session: &mut Session) -> UiNode {
    session
        .hosted()
        .view()
        .and_then(tilo_ui_host::AppWindow::composed)
        .cloned()
        .expect("the window composed its shell")
}

/// What the person does at the control `key` (`press`, `choose`, `select`).
fn fire(session: &mut Session, key: &str, event: &str, value: Option<Value>) {
    session
        .hosted()
        .view()
        .expect("the window is connected")
        .act(UiEvent::Fire(key.to_owned(), event.to_owned(), value));
    session.settle();
}

/// The key of the first node of `kind` whose prop `prop` reads `value`.
fn key_of(tree: &UiNode, kind: &str, prop: &str, value: &str) -> String {
    find(tree, &|node| {
        node.kind == kind && node.props.get(prop).map(display).as_deref() == Some(value)
    })
    .unwrap_or_else(|| panic!("the window shows a {kind} whose {prop} is {value:?}"))
    .key
    .clone()
}

/// Every node of `kind` under `node`.
fn all_of<'a>(node: &'a UiNode, kind: &str, out: &mut Vec<&'a UiNode>) {
    if node.kind == kind {
        out.push(node);
    }
    for child in &node.children {
        all_of(child, kind, out);
    }
}

/// The nodes of the shell's slot `name`.
fn slot_of<'a>(shell: &'a UiNode, name: &str) -> Vec<&'a UiNode> {
    shell
        .children
        .iter()
        .filter(|child| {
            child.kind == tilo_ui_host::adapter::SLOT
                && child.props.get("name") == Some(&Value::text(name))
        })
        .flat_map(|child| child.children.iter())
        .collect()
}

/// The labels of the buttons the shell's slot `name` holds, however deep.
fn buttons_in(shell: &UiNode, name: &str) -> Vec<String> {
    let mut buttons = Vec::new();
    for node in slot_of(shell, name) {
        all_of(node, "ui.Button", &mut buttons);
    }
    buttons
        .into_iter()
        .filter_map(|button| button.props.get("label").map(display))
        .collect()
}

/// The text a prop of the shell reads.
fn prop(shell: &UiNode, name: &str) -> Option<String> {
    shell.props.get(name).map(display)
}

/// Every text the window shows: its views' and the rows of every list it holds, each row asked
/// for.
fn everything_shown(session: &mut Session) -> Vec<String> {
    let lists: Vec<_> = {
        let view = session.hosted().view().expect("the window is connected");
        view.lists()
            .into_iter()
            .filter_map(|list| view.rows_of(&list))
            .collect()
    };
    for rows in &lists {
        let len = rows.borrow().len();
        rows.borrow_mut().request_range(0..len, None);
    }
    session.settle();
    let mut shown = Vec::new();
    let view = session.hosted().view().expect("the window is connected");
    for tree in [view.root_tree(), view.detail_tree()].into_iter().flatten() {
        said(&tree, &mut shown);
    }
    for rows in &lists {
        let rows = rows.borrow();
        for index in 0..rows.len() {
            if let Some(row) = rows.key_at(index).and_then(|key| rows.row(&key).cloned()) {
                said(&row, &mut shown);
            }
        }
    }
    shown
}

/// Every text `node` says, rich text a line per paragraph.
fn said(node: &UiNode, out: &mut Vec<String>) {
    for value in node.props.values() {
        match value {
            Value::Text(text) => out.push(text.clone()),
            Value::Rich(rich) => out.extend(rich.blocks.iter().map(|block| block.text.clone())),
            _ => {}
        }
    }
    for child in &node.children {
        said(child, out);
    }
}

/// The person clicks the row of the tree or list titled `title`.
fn choose_row(session: &mut Session, title: &str) {
    let (x, y) = note_row(session, title);
    session.window.click_at(x, y);
    session.settle();
}

/// The person clicks the sidebar's place `label`.
fn choose_place(session: &mut Session, label: &str) {
    let (x, y) = place_at(session, label);
    session.window.click_at(x, y);
    session.settle();
}

/// The resources the collection `field` of the root of `app` holds, in its order.
fn items_of(session: &mut Session, app: &str, field: &str) -> Vec<ResourceRef> {
    session.settle();
    let host = session.instance_host();
    let root = host.root_of(app).expect("the app's root");
    let Ok(RtValue::List(items)) = host.runtime().read(&root, field) else {
        panic!("{app}'s {field} is a collection");
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            RtValue::Ref(item) => Some((*item.target).clone()),
            _ => None,
        })
        .collect()
}

/// What the resource `at` holds in `field`.
fn read(session: &Session, at: &ResourceRef, field: &str) -> Option<RtValue> {
    session.instance_host().runtime().read(at, field).ok()
}

/// The id of what the window shows beside its list or tree, if it shows one: the resource the
/// page's first bound field edits, in the lane after the list's or in the shell's content column.
fn opened(session: &mut Session) -> Option<String> {
    let view = session.hosted().view()?;
    if let Some(target) = view.detail_target() {
        return Some(target.id_string());
    }
    let root = view.root_tree()?;
    let mut page = Vec::new();
    super::content_nodes(&root, &mut page);
    let (edited, _) = page
        .into_iter()
        .find_map(|node| {
            find(node, &|node| {
                node.binding
                    .as_ref()
                    .and_then(|binding| binding.field())
                    .is_some()
            })
        })?
        .binding
        .as_ref()?
        .field()?;
    // A control names what it edits by the id the app publishes it under: `tl:<app>:<id>`.
    Some(edited.splitn(3, ':').nth(2).unwrap_or(edited).to_owned())
}

/// The titles of the rows the window lists, in the order it lists them, every row asked for as a
/// window tall enough for all of them asks.
fn listed(session: &mut Session) -> Vec<String> {
    let lists: Vec<_> = {
        let view = session.hosted().view().expect("the window is connected");
        view.lists()
            .into_iter()
            .filter(|list| !list.starts_with(tilo_ui_host::shell::DETAIL))
            .filter_map(|list| view.rows_of(&list))
            .collect()
    };
    for rows in &lists {
        let len = rows.borrow().len();
        rows.borrow_mut().request_range(0..len, None);
    }
    session.settle();
    let mut titles = Vec::new();
    for rows in &lists {
        let rows = rows.borrow();
        for index in 0..rows.len() {
            let row = rows.key_at(index).and_then(|key| rows.row(&key).cloned());
            titles.extend(row.as_ref().map(super::title_of));
        }
    }
    titles
}

/// The window's own search, as its search key opens it: where it searches, and its driver.
fn search(session: &mut Session) -> (SearchDriver, InPlace) {
    let in_place = InPlace::of(session.window.root());
    let mut driver = SearchDriver::default();
    driver.open(in_place);
    (driver, in_place)
}

/// The person types `text` into the window's search, or closes it (`close`).
fn type_in_search(session: &mut Session, driver: &mut SearchDriver, text: &str, close: bool) {
    driver.drive(
        session.window.root(),
        text,
        FindOptions::default(),
        None,
        close,
    );
    session.settle();
    // The bar reads the count again once the app answered, as it does every frame it shows.
    if !close {
        driver.drive(
            session.window.root(),
            text,
            FindOptions::default(),
            None,
            false,
        );
    }
}

// ----- the notebook ------------------------------------------------------------------------

/// A notebook's window is its tree, the note chosen in it beside it: the bar offers New note and
/// names the note the page shows, the page edits that note's title as its large title, and the
/// window says which note it shows so a desktop tells two windows of one app apart.
#[test]
fn a_notebook_is_its_tree_with_the_chosen_note_beside_it_naming_the_window() {
    let mut session = staged(NOTES, "a week of notes");
    let shell = session
        .hosted()
        .view()
        .and_then(tilo_ui_host::AppWindow::shell_ref)
        .map(|shell| {
            (
                shell.has_overview(),
                shell.has_sidebar(),
                shell.has_content(),
            )
        })
        .expect("the window's shell");
    assert_eq!(
        shell,
        (false, true, true),
        "the notebook is the sidebar's tree, and the note chosen stands beside it"
    );
    let notes = listed(&mut session);
    assert_eq!(
        notes.first().map(String::as_str),
        Some("Q3 plan"),
        "{notes:?}"
    );
    let shell_tree = composed(&mut session);
    assert_eq!(prop(&shell_tree, "sidebar_title").as_deref(), Some("Notes"));
    assert!(
        buttons_in(&shell_tree, "sidebar_actions").is_empty(),
        "the sidebar's header offers no button of the window's"
    );
    assert!(
        buttons_in(&shell_tree, "actions").contains(&"New note".to_owned()),
        "the window's primary action stands in its bar: {:?}",
        buttons_in(&shell_tree, "actions")
    );
    assert_eq!(
        session.hosted().window_title(),
        Some("Q3 plan"),
        "the window opens on the notebook's first note and is named by it"
    );

    choose_row(&mut session, "Standups");
    assert_eq!(
        session.hosted().window_title(),
        Some("Standups"),
        "the note chosen beside the tree names the window now"
    );
    let shell_tree = composed(&mut session);
    assert_eq!(
        prop(&shell_tree, "title").as_deref(),
        Some("Standups"),
        "the bar names the note the page shows, for when the page's heading scrolls away"
    );
    let title = slot_of(&shell_tree, "content")
        .into_iter()
        .find_map(|node| find(node, &|node| node.kind == "ui.TextInput"))
        .expect("the page edits the note's title");
    assert!(
        matches!(
            title.props.get("title"),
            Some(&Value::Int(1) | &Value::Bool(true))
        ),
        "the page binds the title as its large title: {:?}",
        title.props.get("title")
    );
    // Renaming the note on its page renames its row in the tree.
    let key = title.key.clone();
    session
        .hosted()
        .view()
        .expect("the window is connected")
        .act(UiEvent::Edit(key, "Plans for the week".to_owned()));
    session.settle();
    let notes = listed(&mut session);
    assert!(
        notes.iter().any(|shown| shown == "Plans for the week"),
        "{notes:?}"
    );
}

/// An empty notebook says so once, in place of both columns; its primary action stands once, in
/// the bar, and what it makes opens beside the tree.
#[test]
fn an_empty_notebook_says_so_once_and_its_bar_makes_the_first_note() {
    let mut session = Session::notes();
    let shell_tree = composed(&mut session);
    let mut empty = Vec::new();
    all_of(&shell_tree, "ui.EmptyState", &mut empty);
    assert_eq!(empty.len(), 1, "one state, once: {empty:?}");
    assert_eq!(prop(empty[0], "title").as_deref(), Some("No notes yet"));
    assert!(
        empty
            .iter()
            .all(|state| !state.props.contains_key("action")),
        "the state draws no call to action beside the bar's"
    );
    assert_eq!(
        buttons_in(&shell_tree, "actions")
            .iter()
            .filter(|label| *label == "New note")
            .count(),
        1,
        "the bar offers New note once"
    );
    press_new_note(&mut session);
    session.settle();
    assert_eq!(items_of(&mut session, NOTES, "notes").len(), 1);
    assert_eq!(listed(&mut session), ["Untitled"]);
    assert!(
        opened(&mut session).is_some(),
        "the note it made stands beside the tree"
    );
}

/// What an agent's task or a script makes through the door moves nobody's view: the note the
/// person has open stays open, and only their own press opens what it made.
#[test]
fn what_an_agent_or_a_script_creates_through_the_door_moves_nobodys_view() {
    let mut session = Session::notes();
    let root = session
        .read(|doc| tilolang_tilo::action::resource_ids(doc, NOTES))
        .remove(0);
    let notebook = format!("tl:{NOTES}:{root}");
    let new_note = action_id(&mut session, &notebook, "New note");
    press_new_note(&mut session);
    session.settle();
    let mine = opened(&mut session).expect("the person's note opened");
    // An agent's task makes a note, and so does a script on the control socket, which acts as
    // the person but not at the window.
    let (agent, ambient) = task();
    let script = ActionInvoker::person("ada".to_owned());
    for (invoker, ambient) in [(agent.clone(), Some(&ambient)), (script, None)] {
        session
            .invoke(Some(invoker), ambient, &notebook, &new_note, &Map::new())
            .unwrap();
        session.settle();
        assert_eq!(
            opened(&mut session).as_ref(),
            Some(&mine),
            "the note made through the door did not open"
        );
    }
    assert_eq!(items_of(&mut session, NOTES, "notes").len(), 3);
    // The person pressing New note opens what it made, and an agent's note after it leaves it.
    press_new_note(&mut session);
    session.settle();
    let theirs = opened(&mut session).expect("the person's second note opened");
    assert_ne!(theirs, mine, "the press opened the note it made");
    session
        .invoke(
            Some(agent),
            Some(&ambient),
            &notebook,
            &new_note,
            &Map::new(),
        )
        .unwrap();
    session.settle();
    assert_eq!(items_of(&mut session, NOTES, "notes").len(), 5);
    assert_eq!(
        opened(&mut session),
        Some(theirs),
        "an agent's note leaves the person's open"
    );
}

/// The mark of what an agent's run changed lists each item it made, titled by the item: pressing
/// one opens that note beside the tree.
#[test]
fn a_change_marks_item_opens_the_note_it_names() {
    let mut session = Session::notes();
    let root = session
        .read(|doc| tilolang_tilo::action::resource_ids(doc, NOTES))
        .remove(0);
    let notebook = format!("tl:{NOTES}:{root}");
    let new_note = action_id(&mut session, &notebook, "New note");
    press_new_note(&mut session);
    session.settle();
    let mine = opened(&mut session).expect("the person's note opened");
    let (agent, ambient) = task();
    session
        .invoke(
            Some(agent),
            Some(&ambient),
            &notebook,
            &new_note,
            &Map::new(),
        )
        .unwrap();
    session.settle();
    let item = find(&composed(&mut session), &|node| {
        node.key.ends_with("~change~since-0~item")
    })
    .map(|node| node.key.clone())
    .expect("the mark lists what the run made");
    assert_eq!(
        opened(&mut session).as_ref(),
        Some(&mine),
        "the mark's item does not open itself"
    );
    fire(&mut session, &item, "press", None);
    let shown = opened(&mut session).expect("pressing the mark's item opens it");
    assert_ne!(shown, mine, "the mark opened the note the run made");
    assert!(
        items_of(&mut session, NOTES, "notes")
            .iter()
            .any(|note| note.id_string().ends_with(&shown)),
        "a note of the notebook: {shown}"
    );
}

/// A scenario stages its data once, however many frames the window runs after it.
#[test]
fn a_scenario_stages_exactly_its_data_however_many_frames_run() {
    let mut session = staged(NOTES, "a week of notes");
    let staged = items_of(&mut session, NOTES, "notes").len();
    assert_eq!(staged, 8, "the scenario's notes");
    for _ in 0..6 {
        session.settle();
    }
    assert_eq!(items_of(&mut session, NOTES, "notes").len(), staged);
    assert_eq!(listed(&mut session).len(), staged);
    assert_eq!(
        session
            .read(|doc| tilolang_tilo::action::resource_ids(doc, NOTES))
            .len(),
        1,
        "one notebook"
    );
}

/// The window's search narrows the tree to the notes it finds, with the notes they stand under,
/// finds a note by the words of its body, and widens back to the tree as it was when it closes.
#[test]
fn typing_in_the_window_s_search_narrows_a_tree_to_what_matches() {
    let mut session = staged(NOTES, "a week of notes");
    let notebook = listed(&mut session);
    assert!(notebook.len() > 4, "the notebook's tree: {notebook:?}");
    let (mut driver, in_place) = search(&mut session);
    assert_eq!(
        in_place,
        InPlace::Query,
        "the frame says where the search goes, so typing narrows the tree in place"
    );
    type_in_search(&mut session, &mut driver, "Monday", false);
    assert_eq!(
        listed(&mut session),
        ["Q3 plan", "Standups", "Monday"],
        "the note the words name, and the notes it stands under"
    );
    assert_eq!(
        driver.count(),
        Some(FilterCount::new(3, notebook.len())),
        "the search says how many rows it left of how many the tree showed"
    );
    type_in_search(&mut session, &mut driver, "sync bug", false);
    assert_eq!(
        listed(&mut session),
        ["Q3 plan", "Standups", "Monday"],
        "the note whose body says it, with the way to it"
    );
    type_in_search(&mut session, &mut driver, "", true);
    assert_eq!(
        listed(&mut session),
        notebook,
        "the tree widens back to the notebook's notes"
    );
}

/// Find in an open note counts its matches and steps through them; replacing all of them is one
/// change, one step to undo.
#[test]
fn find_in_an_open_note_counts_its_matches_and_replaces_them_all_in_one_change() {
    let mut session = Session::notes();
    // What the person types is one step once they pause, as a pause ends a burst of typing.
    super::store(&session).set_edit_pause(std::time::Duration::from_millis(200));
    super::new_note(&mut session, "Cargo");
    session
        .window
        .type_text("Ship the boat. Ship the crew. Ship the cargo.");
    session.settle();
    assert_eq!(
        super::body_text(&mut session),
        "Ship the boat. Ship the crew. Ship the cargo."
    );
    // The person stops typing before they search: what they typed is a step of its own.
    std::thread::sleep(std::time::Duration::from_millis(400));
    let (mut driver, _) = search(&mut session);
    let mut find_step = |session: &mut Session, step| {
        driver.drive(
            session.window.root(),
            "Ship",
            FindOptions::default(),
            step,
            false,
        );
        session.settle();
        driver.position()
    };
    assert_eq!(
        find_step(&mut session, None),
        Some((1, 3)),
        "the note holds three, the first is current"
    );
    assert_eq!(find_step(&mut session, Some(true)), Some((2, 3)), "next");
    assert_eq!(
        find_step(&mut session, Some(false)),
        Some((1, 3)),
        "previous"
    );
    assert!(
        driver.replace(
            session.window.root(),
            "Ship",
            FindOptions::default(),
            "Launch",
            true,
        ),
        "the replacement was taken"
    );
    session.settle();
    assert_eq!(
        super::body_text(&mut session),
        "Launch the boat. Launch the crew. Launch the cargo.",
        "every match replaced"
    );
    // The replacement is one step: one undo takes every match back.
    session.window.chord(&["ctrl"], "z");
    session.settle();
    assert_eq!(
        super::body_text(&mut session),
        "Ship the boat. Ship the crew. Ship the cargo.",
        "one undo takes the whole replacement back"
    );
}

/// A note that left with the note it stood under is gone: nothing of it reads, and opening it
/// shows none of what it held.
#[test]
fn a_note_that_left_with_the_note_it_stood_under_shows_nothing_of_it() {
    let mut session = staged(NOTES, "a week of notes");
    let notes = items_of(&mut session, NOTES, "notes");
    let parent_of = |session: &Session, note: &ResourceRef| match read(session, note, "parent") {
        Some(RtValue::Opt(Some(held))) => match *held {
            RtValue::Ref(parent) => Some((*parent.target).clone()),
            _ => None,
        },
        _ => None,
    };
    let (parent, under) = notes
        .iter()
        .find_map(|note| parent_of(&session, note).map(|parent| (parent, note.clone())))
        .expect("a note of the scenario stands under another");
    let Some(RtValue::Text(title)) = read(&session, &under, "title") else {
        panic!("a note is titled");
    };
    let host = session.instance_host();
    let notebook = host.root_of(NOTES).expect("the notebook");
    let delete = host
        .package(NOTES)
        .and_then(|package| {
            package
                .meta
                .actions
                .iter()
                .find(|action| action.name == "delete")
                .map(|action| action.id.clone())
        })
        .expect("Notes declares `delete`");
    host.runtime()
        .invoke_action(
            &Principal::new("ada"),
            (
                delete,
                vec![RtValue::resource(&notebook), RtValue::resource(&parent)],
            ),
        )
        .expect("the person deletes the note");
    session.settle();
    assert!(
        !items_of(&mut session, NOTES, "notes").contains(&under),
        "the note went with the one it stood under"
    );
    assert!(
        read(&session, &under, "title").is_none(),
        "nothing of the note reads"
    );
    session.instance_mut().reveal(under.id_string());
    session.settle();
    assert!(
        !everything_shown(&mut session).contains(&title),
        "the page of a note that is gone shows no title"
    );
}

// ----- a list's search ---------------------------------------------------------------------

/// The window's search narrows a project's tasks to the ones whose title or notes say the words,
/// and widens back to the project's tasks when it closes.
#[test]
fn typing_in_the_window_s_search_narrows_a_projects_tasks_to_what_matches() {
    let mut session = staged(TASKS, "a busy week");
    choose_place(&mut session, "Website launch");
    let before = listed(&mut session);
    assert!(before.len() > 3, "the project lists its tasks: {before:?}");
    let (mut driver, in_place) = search(&mut session);
    assert_eq!(
        in_place,
        InPlace::Query,
        "the window narrows its list in place"
    );
    type_in_search(&mut session, &mut driver, "pricing", false);
    let found = listed(&mut session);
    assert_eq!(found.len(), 1, "one task says it: {found:?}");
    assert!(found[0].contains("pricing"), "{found:?}");
    type_in_search(&mut session, &mut driver, "offline", false);
    let found = listed(&mut session);
    assert_eq!(found.len(), 1, "the task whose notes say it: {found:?}");
    assert!(found[0].contains("launch post"), "{found:?}");
    type_in_search(&mut session, &mut driver, "", true);
    assert_eq!(
        listed(&mut session),
        before,
        "the list widens back to the place's tasks"
    );
}

// ----- chat --------------------------------------------------------------------------------

/// The places of the sidebar under `node`, in order.
fn places(node: &UiNode, out: &mut Vec<String>) {
    let mut items = Vec::new();
    all_of(node, "ui.NavItem", &mut items);
    out.extend(
        items
            .into_iter()
            .filter_map(|item| item.props.get("label").map(display)),
    );
}

/// Each heading of the sidebar and its places.
fn sections(node: &UiNode) -> Vec<(String, Vec<String>)> {
    let mut found = Vec::new();
    all_of(node, "ui.NavSection", &mut found);
    found
        .into_iter()
        .map(|section| {
            let mut held = Vec::new();
            places(section, &mut held);
            (
                section.props.get("title").map(display).unwrap_or_default(),
                held,
            )
        })
        .collect()
}

/// A workspace with no channels says so once, offers New channel once, and the channel it makes
/// is in the sidebar before anyone writes in it; a workspace's channels stand under their
/// categories, with the people to write to under a heading of their own.
#[test]
fn a_new_channel_is_in_the_sidebar_before_anyone_writes_and_every_channel_under_its_category() {
    let mut session = Session::new(CHAT, "ada");
    let shell = composed(&mut session);
    assert_eq!(
        shell.props.get("nothing_listed"),
        Some(&Value::Bool(true)),
        "the window says it lists nothing"
    );
    assert!(
        slot_of(&shell, "sidebar").is_empty(),
        "the places draw no second state"
    );
    let mut empty = Vec::new();
    all_of(&shell, "ui.EmptyState", &mut empty);
    assert_eq!(empty.len(), 1, "one state, once: {empty:?}");
    assert_eq!(prop(empty[0], "title").as_deref(), Some("No channels yet"));
    assert!(
        empty[0].props.contains_key("message"),
        "with the line that says what a channel is for"
    );
    let new_channel = key_of(&shell, "ui.Button", "label", "New channel");
    fire(&mut session, &new_channel, "press", None);
    assert_eq!(items_of(&mut session, CHAT, "channels").len(), 1);
    let shell = composed(&mut session);
    let item = find(&shell, &|node| {
        node.kind == "ui.NavItem"
            && node.props.get("label").map(display).as_deref() == Some("new-channel")
    })
    .expect("the new channel is in the sidebar, no message written");
    assert_eq!(
        item.props.get("icon").map(display).as_deref(),
        Some("hash"),
        "with the channels' glyph"
    );

    let mut team = staged(CHAT, "a team's channels");
    let shell = composed(&mut team);
    let grouped = sections(&shell);
    let named = |title: &str| {
        grouped
            .iter()
            .find(|(heading, _)| heading == title)
            .map_or_else(
                || panic!("the sidebar has a {title} heading: {grouped:?}"),
                |(_, held)| held.clone(),
            )
    };
    assert_eq!(named("Team"), ["design", "release", "planning"]);
    assert_eq!(named("Social"), ["weekend", "books"]);
    assert_eq!(
        named("Direct messages"),
        ["Mia", "Leo", "Ada"],
        "the people the workspace knows, each a conversation to open"
    );
    let mut all = Vec::new();
    places(&shell, &mut all);
    assert_eq!(
        all.get(..2),
        Some(&["general".to_owned(), "random".to_owned()][..]),
        "the channels outside any category first: {all:?}"
    );
}

/// Search finds a chat message by its words, its index keeps only short fields, and the hit
/// opens the channel the message is in.
#[test]
fn search_finds_chat_messages_and_a_hit_opens_their_channel() {
    let mut session = staged(CHAT, "a team's channels");
    let said = "Coffee machine on the third floor is fixed.";
    assert_ne!(
        session.hosted().window_title(),
        Some("general"),
        "the scenario opens another channel"
    );
    let entries: Vec<_> = session
        .index_deltas()
        .into_iter()
        .flat_map(|delta| delta.upsert)
        .collect();
    let hit = entries
        .iter()
        .find(|entry| entry.title.contains("third floor"))
        .unwrap_or_else(|| panic!("a message is found: {entries:#?}"))
        .resource
        .clone();
    session.instance_mut().reveal(hit);
    session.settle();
    assert_eq!(
        session.hosted().window_title(),
        Some("general"),
        "the hit opened the channel the message is in"
    );
    assert!(
        everything_shown(&mut session)
            .iter()
            .any(|text| text.contains(said)),
        "which shows the message"
    );
}

// ----- the door ----------------------------------------------------------------------------

/// What the host's operation labelled `label` answers an agent's task given `arguments`.
fn agent_runs(session: &mut Session, label: &str, arguments: &Json) -> Result<Json, String> {
    let action = action_id(session, HOST, label);
    let (agent, ambient) = task();
    session
        .invoke(
            Some(agent),
            Some(&ambient),
            HOST,
            &action,
            arguments.as_object().expect("arguments are an object"),
        )
        .map(|output| output.value.unwrap_or_default())
}

/// An agent finds the host's operations at the door: the catalogue names every installed app's
/// actions, and a block answers as a view, as a value, or with why it does not compile.
#[test]
fn an_agent_reads_the_catalogue_and_a_view_block_a_value_block_and_a_bad_block_answer_it() {
    let mut session = Session::notes();
    let catalogue = agent_runs(&mut session, "List catalogue", &json!({})).unwrap();
    assert!(
        catalogue.to_string().contains(NOTES),
        "the catalogue names the notebook's actions: {catalogue}"
    );
    let run = |session: &mut Session, source: &str| {
        agent_runs(session, "Run a block", &json!({ "source": source }))
    };
    let view = run(&mut session, "stack { text(\"Hello from an agent\") }").unwrap();
    assert_eq!(view["kind"], "view", "{view}");
    let data = run(&mut session, "1 + 2").unwrap();
    assert_eq!(data, json!({"kind": "data", "value": 3}));
    let refused = run(&mut session, "nothing.here(").unwrap_err();
    assert!(refused.contains("does not compile"), "{refused}");
}

/// The first draft on the letters desk.
fn first_draft(session: &mut Session) -> ResourceRef {
    items_of(session, LETTERS, "drafts")
        .into_iter()
        .next()
        .expect("the desk holds a draft")
}

/// Whether the letter went, as the record says: no device sends it again.
fn sent(session: &mut Session, draft: &ResourceRef) -> bool {
    session.settle();
    read(session, draft, "sent") == Some(RtValue::Bool(true))
}

/// A letter an agent sends is a suggestion the person decides: the agent cannot approve it,
/// nothing leaves before the person does, and approving it sends it once.
#[test]
fn a_person_approves_an_agents_suggested_send_through_the_door_and_it_leaves_once() {
    let mut session = staged(LETTERS, "a desk of letters");
    let draft = first_draft(&mut session);
    let source = format!("letters.send(@letters.Draft:{})", draft.id_string());
    let suggested = agent_runs(&mut session, "Run a block", &json!({ "source": source })).unwrap();
    assert_eq!(suggested["kind"], "invocation", "{suggested}");
    assert_eq!(
        suggested["state"], "suggested",
        "an agent's send asks first: {suggested}"
    );
    let id = suggested["invocation"].as_str().unwrap().to_owned();
    let host = session.instance_host();
    let record = host
        .runtime()
        .read_invocation(&host.invocation_ref(&id).unwrap())
        .unwrap();
    assert_eq!(
        record.author,
        Principal::new(task().0.key()),
        "the suggestion is the agent's"
    );
    assert!(!sent(&mut session, &draft), "nothing left");
    let decide = json!({"target": id, "decision": "approve"});
    let refused = agent_runs(&mut session, "Decide suggestion", &decide).unwrap_err();
    assert!(
        refused.contains("only a person"),
        "agents never decide: {refused}"
    );
    assert!(!sent(&mut session, &draft), "nothing left");
    let resolve = action_id(&mut session, HOST, "Decide suggestion");
    let person = |session: &mut Session| {
        session.invoke(
            None,
            None,
            HOST,
            &resolve,
            decide.as_object().expect("arguments"),
        )
    };
    let decided = person(&mut session).unwrap().value.unwrap_or_default();
    assert_eq!(decided["state"], "done", "{decided}");
    assert!(sent(&mut session, &draft), "the letter went");
    assert!(
        person(&mut session).is_err(),
        "approving again changes nothing: the suggestion is decided"
    );
    assert!(
        tilolang_tilo::runtime::store::Store::outbox_pending(session.instance_host().store())
            .is_empty(),
        "nothing waits to leave again"
    );
}

/// The person's own Send asks them first, showing the letter, and goes once they allow it.
#[test]
fn approving_a_pressed_send_through_the_ask_bar_sends_the_letter_once() {
    let mut session = staged(LETTERS, "a desk of letters");
    let draft = opened(&mut session).expect("the scenario opens its letter");
    let draft = items_of(&mut session, LETTERS, "drafts")
        .into_iter()
        .find(|letter| letter.id_string() == draft)
        .expect("the open letter is on the desk");
    let send = key_of(&composed(&mut session), "ui.MenuItem", "label", "Send");
    fire(&mut session, &send, "choose", None);
    assert!(
        session
            .hosted()
            .view()
            .and_then(tilo_ui_host::AppWindow::asks)
            .is_some(),
        "a send asks the person first"
    );
    assert!(
        !sent(&mut session, &draft),
        "nothing leaves before they say so"
    );
    session
        .hosted()
        .view()
        .expect("the window is connected")
        .answer(tilo_render_protocol::patch::Answer::Allow);
    session.settle();
    assert!(sent(&mut session, &draft), "the letter went");
    session.settle();
    assert!(
        tilolang_tilo::runtime::store::Store::outbox_pending(session.instance_host().store())
            .is_empty(),
        "the record says it was delivered, so no device queues it again"
    );
}

// USER REQUIREMENT: a note the workspace cannot keep is said once, and the window waits for the
// person's next step instead of trying again every frame: no spinning CPU, no drained battery.
// Once the workspace takes changes again, the same press makes the note.
#[test]
fn a_note_the_workspace_cannot_keep_is_said_once_and_the_window_rests() {
    let mut session = Session::notes();
    session.settle();
    let notes = session.titles().len();
    let store = session.instance_host().store().clone();
    let note = session
        .instance_host()
        .package(&session.app)
        .expect("the notes package")
        .meta
        .type_named("Note")
        .expect("Notes keeps notes")
        .id
        .clone();
    store.fail_commits_adding(&note);
    let plus = session.control(&session.doing("Notebook", "new_note"));
    session.window.click_at(plus.0, plus.1);
    // The host comes to rest: `settle` returns only once a turn reports no change.
    session.settle();
    let turns_at_rest = (0..5).filter(|_| session.host.turn()).count();
    assert_eq!(
        turns_at_rest, 0,
        "the window rests while the workspace refuses"
    );
    assert_eq!(
        session.titles().len(),
        notes,
        "no note stands that the workspace did not keep"
    );
    let said = ui::window_toolbar::notice().expect("the window says the note was not kept");
    assert_eq!(said.kind, ui::widget::toast::ToastKind::Error, "{said:?}");
    assert!(said.text.contains("refused"), "it says why: {said:?}");

    store.take_commits();
    let plus = session.control(&session.doing("Notebook", "new_note"));
    session.window.click_at(plus.0, plus.1);
    session.settle();
    assert_eq!(
        session.titles().len(),
        notes + 1,
        "the press makes the note now"
    );
}
