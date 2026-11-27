//! What a row's menu offers a person, a tool and a screen reader, in a live window: the
//! commands it names run the one way a click does, where they are pressed.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "pixels of a small frame: coordinates are tiny"
)]

use super::*;
use tilo_app_window::reconcile::display;

/// A task's row menu runs the command it names: Complete marks the task done, whether it is
/// pressed where it is drawn or by its name, and the menu closes both times — while the tasks
/// the person did not touch stay rows.
#[test]
fn a_tasks_menu_marks_it_done_by_its_complete_command() {
    let _settled = ui::motion::testing::settled();
    let surface = package(package_of(TASKS))
        .surface()
        .expect("the package has a surface");
    collab_types::app_surface::register_package(TASKS, &surface);

    // Pressed where it is drawn.
    let mut session = live_task("Water the plants");
    add_task(&mut session, "Call the plumber");
    assert!(row_menu(&mut session, "Water the plants").contains(&"Complete".to_owned()));
    let rect = session
        .window
        .menu_item("Complete")
        .expect("Complete in the row's menu")
        .1
        .bounds()
        .expect("the command is laid out");
    session.window.click_at(
        f64::midpoint(rect.x0, rect.x1),
        f64::midpoint(rect.y0, rect.y1),
    );
    session.settle();
    assert!(
        task_in_workspace(&session, "Water the plants", true),
        "Complete marked the task done in the workspace"
    );
    assert!(
        session.window.menu_item("Complete").is_none(),
        "the menu closed on its command"
    );
    assert!(
        session.window.node_named("Call the plumber").is_some(),
        "the task nobody touched is still a row: titles {:?} controls {:?}",
        session.titles(),
        controls_of(session.window.app())
    );

    // Pressed by its name, as a tool or a screen reader presses it.
    let mut session = live_task("Water the plants");
    assert!(row_menu(&mut session, "Water the plants").contains(&"Complete".to_owned()));
    let (_, shown) = session
        .window
        .menu_item("Complete")
        .expect("Complete in the menu");
    assert!(
        session
            .window
            .app()
            .window_controls()
            .iter()
            .any(|control| control.name == "Complete"),
        "a tool reaches the command by name"
    );
    let rect = shown.bounds().expect("the command is laid out");
    session.window.click_at(
        f64::midpoint(rect.x0, rect.x1),
        f64::midpoint(rect.y0, rect.y1),
    );
    session.settle();
    assert!(
        task_in_workspace(&session, "Water the plants", true),
        "Complete by name marked the task done in the workspace"
    );
    assert!(
        session.window.menu_item("Complete").is_none(),
        "the menu closed on its command"
    );
    assert_eq!(shown.role(), ui::accesskit::Role::MenuItem);
}

/// A key a task's actions answer to runs on the row that has the keyboard, though the person
/// never opened its menu and so the window holds none of the row's commands: Ctrl+Enter
/// completes the task, and no menu opens for it.
#[test]
fn a_tasks_key_completes_it_from_a_row_whose_menu_never_opened() {
    let _settled = ui::motion::testing::settled();
    let surface = package(package_of(TASKS))
        .surface()
        .expect("the package has a surface");
    collab_types::app_surface::register_package(TASKS, &surface);
    let mut session = Session::tasks();
    choose(&mut session, "New task");
    session.window.type_text("Water the plants");
    session
        .window
        .press_key(ui::winit::keyboard::NamedKey::Enter);
    session.settle();
    // From the entry the task was typed into, Tab gives the list the keyboard, and Down puts it
    // on the task's row.
    session.window.harness().key("Tab");
    session.settle();
    session.window.harness().key("Down");
    session.settle();
    let focused = session.window.harness().inspection();
    assert!(
        focused.root.nodes().iter().any(|node| {
            node.widget == "SwipeRow"
                && node
                    .semantics
                    .as_ref()
                    .is_some_and(|semantics| semantics.state.focused)
        }),
        "the keyboard is on the task's row, not on {:?}",
        focused
            .root
            .nodes()
            .iter()
            .filter(|node| node
                .semantics
                .as_ref()
                .is_some_and(|semantics| semantics.state.focused))
            .map(|node| (
                node.widget.clone(),
                node.semantics.as_ref().and_then(|s| s.name.clone())
            ))
            .collect::<Vec<_>>()
    );
    session.window.harness().key("Ctrl+Enter");
    session.settle();
    assert!(
        task_in_workspace(&session, "Water the plants", true),
        "Ctrl+Enter on the row completed its task"
    );
    assert_eq!(
        menu_commands(&mut session),
        Vec::<String>::new(),
        "no menu opened"
    );
}

/// Another task typed into the window's entry, after the first.
fn add_task(session: &mut Session, title: &str) {
    choose(session, "New task");
    session.window.type_text(title);
    session
        .window
        .press_key(ui::winit::keyboard::NamedKey::Enter);
    session.settle();
}

/// Every command a task's menu offers acts when a tool presses it by its name: each is pressed
/// where it stands, the menu closes on it, and what it names follows — the task is done or gone,
/// or the surface it opens (a date, who does it, the commands behind a run) stands in its place.
/// The commands the view gives a run of their own open from the run's row, and stand under it.
#[test]
fn every_command_of_a_tasks_menu_acts_by_its_name() {
    let _settled = ui::motion::testing::settled();
    let surface = package(package_of(TASKS))
        .surface()
        .expect("the package has a surface");
    collab_types::app_surface::register_package(TASKS, &surface);
    for (run, command) in [
        (None, "Open as page"),
        (None, "Complete"),
        (Some("Plan"), "Pick date…"),
        (Some("Priority"), "Low"),
        (Some("More"), "Duplicate"),
        (Some("More"), "Delete"),
    ] {
        let mut session = live_task("Water the plants");
        let menu = row_menu(&mut session, "Water the plants");
        let top = run.unwrap_or(command);
        assert!(
            menu.contains(&top.to_owned()),
            "the row's menu offers {top}: {menu:?}"
        );
        if let Some(run) = run {
            session.window.press_named(run);
            session.settle();
            let opened = controls_of(session.window.app());
            assert!(
                opened.iter().any(|name| name == command),
                "{run} offers {command}: {opened:?}"
            );
        }
        let before = controls_of(session.window.app());
        let tasks_before = session.titles().len();
        session.window.press_named(command);
        session.settle();
        let after = controls_of(session.window.app());
        assert!(
            after.iter().all(|name| name != command),
            "{command} left the menu open: {after:?}"
        );
        let followed = task_in_workspace(&session, "Water the plants", true)
            || !task_in_workspace(&session, "Water the plants", false)
            || session.titles().len() != tasks_before
            || after
                .iter()
                .any(|name| !before.contains(name) && !menu.contains(name));
        assert!(
            followed,
            "{command} was pressed and nothing followed it: {after:?}"
        );
    }

    // The commands behind More stand beside the menu it was taken from, and one of them acts.
    let mut session = live_task("Water the plants");
    row_menu(&mut session, "Water the plants");
    session.window.press_named("More");
    session.settle();
    let more = controls_of(session.window.app());
    assert!(
        more.iter().any(|name| name == "Move to…"),
        "More shows the commands the menu kept behind it: {more:?}"
    );
    assert_eq!(
        session.titles().len(),
        1,
        "one task before the command behind More"
    );
    session.window.press_named("Duplicate");
    session.settle();
    assert_eq!(
        session.titles().len(),
        2,
        "the command chosen from More ran: it made a second task"
    );
    let after = controls_of(session.window.app());
    assert!(
        after
            .iter()
            .all(|name| name != "Duplicate" && name != "Move to…"),
        "the menu closed on the command chosen from More: {after:?}"
    );
}

/// The commands of the menu the window shows now, as a tool and a screen reader read them.
fn menu_commands(session: &mut Session) -> Vec<String> {
    session
        .window
        .a11y()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::MenuItem)
        .filter_map(|(_, node)| node.label().map(|label| label.trim().to_owned()))
        .collect()
}

/// The commands of the menu of the row `title`, opened where the row stands, as a tool or a
/// screen reader asks for it.
pub(super) fn row_menu(session: &mut Session, title: &str) -> Vec<String> {
    session.window.show_context_menu(title);
    session.settle();
    // The menu the app offers on its row: each run of commands under one group opens with the
    // group's own row, as the person reads it, and the platform's offers close it.
    let row = super::window::row(session, title);
    let mut menu = Vec::new();
    let mut group: Option<String> = None;
    for item in row.of_kind("ui.MenuItem") {
        let item_group = item.props.get("group").map(display);
        if let Some(name) = &item_group
            && group.as_deref() != Some(name.as_str())
        {
            menu.push(name.clone());
        }
        group = item_group;
        if let Some(label) = item.props.get("label").map(display) {
            menu.push(label);
        }
    }
    menu
}

/// Whether `item` is one of the platform's offers on every resource (Export…, Copy link): a row
/// that runs nothing the app publishes, neither an action of its own nor a state of its views.
pub(super) fn is_the_platforms_offer(item: &tilo_app_window::UiNode) -> bool {
    item.events
        .get("choose")
        .is_some_and(|event| event.invokes.is_none() && event.toggles.is_none())
}
