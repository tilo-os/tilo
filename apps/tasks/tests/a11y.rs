//! What a screen reader gets from the task list: every row a named item it
//! can open, tick to finish the issue, and open a menu on — all on the same
//! actions the pointer uses.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::json;
use tasks::App;
use tasks::model::Status;
use ui::accesskit::{Action as AkAction, NodeId, Role, Toggled};
use ui::framework::testing::TestApp;

fn open() -> TestApp<App> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<App>::for_person("ada").expect("open")
}

/// The exported node `app` publishes for the control named `name` with
/// `role`, as an assistive technology finds it.
fn named(app: &mut TestApp<App>, name: &str, role: Role) -> ui::accesskit::Node {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name) && node.role() == role)
        .unwrap_or_else(|| panic!("a {role:?} named {name:?}"))
        .1
}

fn id_of(app: &mut TestApp<App>, name: &str, role: Role) -> NodeId {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some(name) && node.role() == role)
        .unwrap_or_else(|| panic!("a {role:?} named {name:?}"))
        .0
}

fn status_of(app: &TestApp<App>, title: &str) -> Status {
    app.state()
        .issues
        .iter()
        .find(|(_, issue)| issue.title == title)
        .map(|(_, issue)| issue.status)
        .expect("the issue")
}

fn titles(app: &TestApp<App>) -> Vec<String> {
    app.state()
        .issues
        .iter()
        .map(|(_, issue)| issue.title.clone())
        .collect()
}

fn open_menu(app: &mut TestApp<App>, row: &str) {
    let row = id_of(app, row, Role::TreeItem);
    app.harness()
        .perform_a11y_action(row, &ui::a11y::Action::ShowContextMenu)
        .expect("the row's menu opens");
    app.settle();
}

fn choose(app: &mut TestApp<App>, item: &str) {
    let item = id_of(app, item, Role::MenuItem);
    app.harness()
        .perform_a11y_action(item, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("{item:?} is chosen: {error}"));
    app.settle();
}

/// The list publishes each issue as a row a reader reads, opens, finishes
/// and acts on.
#[test]
fn every_task_row_is_a_named_item_a_reader_opens_and_finishes() {
    let mut app = open();
    for title in ["Write the launch notes", "Book the room", "Ship it"] {
        app.call("create", json!({ "title": title }))
            .unwrap_or_else(|error| panic!("create {title}: {error:?}"));
    }
    app.settle();

    // Each row is a tree item named as the list reads, with its spoken
    // status, the state of its box, and the actions a press on it and a
    // right click make.
    for (number, title) in [(1, "Write the launch notes"), (2, "Book the room")] {
        let name = format!("#{number}  {title}");
        let row = named(&mut app, &name, Role::TreeItem);
        assert!(
            row.supports_action(AkAction::Click),
            "{name:?} cannot be opened"
        );
        assert!(
            row.supports_action(AkAction::ShowContextMenu),
            "{name:?} offers no menu"
        );
        assert!(
            row.description().is_some_and(|said| said.contains('#')),
            "{name:?} says no status: {:?}",
            row.description()
        );
        assert_eq!(
            row.toggled(),
            Some(Toggled::False),
            "{name:?} reads unfinished"
        );
    }

    // Ticking a row's box finishes the issue without moving it.
    let row_id = id_of(&mut app, "#1  Write the launch notes", Role::TreeItem);
    app.harness()
        .perform_a11y_action(row_id, &ui::a11y::Action::ToggleCheck)
        .expect("the box toggles");
    app.settle();
    assert_eq!(status_of(&app, "Write the launch notes"), Status::Done);
    assert_eq!(
        titles(&app),
        ["Write the launch notes", "Book the room", "Ship it"],
        "finishing an issue keeps its place"
    );
    let row = named(&mut app, "#1  Write the launch notes", Role::TreeItem);
    assert_eq!(row.toggled(), Some(Toggled::True), "the row reads done");
    assert!(
        row.description().is_some_and(|said| said.contains("Done")),
        "the row reads done: {:?}",
        row.description()
    );

    // The row's menu offers opening the issue and finishing it, and
    // reopening it once it is done.
    open_menu(&mut app, "#2  Book the room");
    choose(&mut app, "Complete");
    assert_eq!(status_of(&app, "Book the room"), Status::Done);
    open_menu(&mut app, "#2  Book the room");
    choose(&mut app, "Reopen");
    assert_eq!(status_of(&app, "Book the room"), Status::Todo);

    // The menu's first item is the row's own open: the issue's page.
    open_menu(&mut app, "#3  Ship it");
    choose(&mut app, "Open issue");
    assert!(
        app.harness().targets().contains_key("tasks-issue-title"),
        "the menu opened the issue's page"
    );
    assert!(
        app.harness()
            .a11y_nodes()
            .into_iter()
            .any(|(_, node)| node.role() == Role::TextInput
                && node.label() == Some("Title")
                && node.value() == Some("Ship it")),
        "and the page is the issue that was opened"
    );
}
