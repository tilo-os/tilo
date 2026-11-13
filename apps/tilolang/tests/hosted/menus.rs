//! What a row's menu offers a person, a tool and a screen reader, in a live window: the
//! commands it names run the one way a click does, where they are pressed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a test: a failure is a panic"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "pixels of a small frame: coordinates are tiny"
)]

use super::*;

/// A task's row menu runs the command it names: Complete marks the task done, whether it is
/// pressed where it is drawn or by its name, and the menu closes both times — while the tasks
/// the person did not touch stay rows.
#[test]
fn a_tasks_menu_marks_it_done_by_its_complete_command() {
    let _settled = ui::motion::testing::settled();
    let surface = package(package_of(TASKS))
        .surface()
        .expect("the package has a surface");
    collaboration::app_surface::register_package(TASKS, &surface);

    // Pressed where it is drawn.
    let mut session = live_task("Water the plants");
    add_task(&mut session, "Call the plumber");
    assert!(task_menu(&mut session, "Water the plants").contains(&"Complete".to_owned()));
    let rect = menu_item(session.window.app(), "Complete")
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
        task_in_record(&session, "Water the plants", true),
        "Complete marked the task done in the workspace"
    );
    assert!(
        menu_item(session.window.app(), "Complete").is_none(),
        "the menu closed on its command"
    );
    assert!(
        described(session.window.app(), "Call the plumber").is_some(),
        "the task nobody touched is still a row: titles {:?} controls {:?}",
        session.titles(),
        controls_of(session.window.app())
    );

    // Pressed by its name, as a tool or a screen reader presses it.
    let mut session = live_task("Water the plants");
    assert!(task_menu(&mut session, "Water the plants").contains(&"Complete".to_owned()));
    let (entry, shown) = menu_item(session.window.app(), "Complete").expect("Complete in the menu");
    assert!(
        session
            .window
            .app()
            .window_controls()
            .iter()
            .any(|control| control.name == "Complete"),
        "a tool reaches the command by name"
    );
    if let Some((x, y)) = session
        .window
        .app()
        .dispatch_a11y_action(entry, &ui::a11y::Action::Activate)
    {
        session.window.click_at(x, y);
    }
    session.settle();
    assert!(
        task_in_record(&session, "Water the plants", true),
        "Complete by name marked the task done in the workspace"
    );
    assert!(
        menu_item(session.window.app(), "Complete").is_none(),
        "the menu closed on its command"
    );
    assert_eq!(shown.role(), ui::accesskit::Role::MenuItem);
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
/// or the surface it opens (a date, who does it, the commands behind More) stands in its place.
#[test]
fn every_command_of_a_tasks_menu_acts_by_its_name() {
    let _settled = ui::motion::testing::settled();
    let surface = package(package_of(TASKS))
        .surface()
        .expect("the package has a surface");
    collaboration::app_surface::register_package(TASKS, &surface);
    for command in [
        "Open as page",
        "Complete",
        "Pick date…",
        "Assign…",
        "Delete",
    ] {
        let mut session = live_task("Water the plants");
        let menu = task_menu(&mut session, "Water the plants");
        assert!(
            menu.contains(&command.to_owned()),
            "the row's menu offers {command}: {menu:?}"
        );
        let before = controls_of(session.window.app());
        press_named(&mut session, command);
        session.settle();
        let after = controls_of(session.window.app());
        assert!(
            after.iter().all(|name| name != command),
            "{command} left the menu standing: {after:?}"
        );
        let followed = task_in_record(&session, "Water the plants", true)
            || !task_in_record(&session, "Water the plants", false)
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
    task_menu(&mut session, "Water the plants");
    press_named(&mut session, "More");
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
    press_named(&mut session, "Duplicate");
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

/// Press the control `name` where it is drawn, as the desk presses a control a tool names: the
/// window acts on the node's own box, so the same press reaches a row of a menu.
fn press_named(session: &mut Session, name: &str) {
    let (node, _) = menu_or_control(session.window.app(), name);
    if let Some((x, y)) = session
        .window
        .app()
        .dispatch_a11y_action(node, &ui::a11y::Action::Activate)
    {
        session.window.click_at(x, y);
    }
}

/// The node named `name` the window draws, the menu's own row first: a page behind the menu may
/// name the same word.
fn menu_or_control(app: &mut ui::App, name: &str) -> (ui::accesskit::NodeId, ui::accesskit::Node) {
    menu_item(app, name)
        .or_else(|| described(app, name))
        .unwrap_or_else(|| panic!("the window draws no control named {name}"))
}

/// The node of the command `command` of the menu standing open, as a tool reads it. A page's
/// button may say the same word, so only a menu's row counts.
fn menu_item(
    app: &mut ui::App,
    command: &str,
) -> Option<(ui::accesskit::NodeId, ui::accesskit::Node)> {
    app.build_a11y_tree_update(1.0)
        .nodes
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::MenuItem
                && node.label().map(str::trim) == Some(command)
        })
}

/// The names of the commands of the row `title`'s menu, opened where the row stands.
fn task_menu(session: &mut Session, title: &str) -> Vec<String> {
    let (row, _) = described(session.window.app(), title).expect("the row");
    session
        .window
        .app()
        .dispatch_a11y_action(row, &ui::a11y::Action::ShowContextMenu);
    session.settle();
    controls_of(session.window.app())
}
