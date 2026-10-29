//! An action waiting for its arguments opens its form (forms-and-diagrams.md §1), in Tasks as a
//! person uses it: a task's menu offers "Snooze until…" and "Assign to…", whose parameters
//! nobody gave and that have no default; choosing one shows its form over the window, the
//! submit stays disabled until the day reads, what was typed survives closing the form, and
//! submit runs the action once.
//!
//! With `TILO_FORMS_SHOTS=<dir>` the test `shots` writes the open form at desktop and phone
//! sizes, light and dark.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

mod support;

use tilolang_app::app::TlApp;
use ui::framework::testing::TestApp;
use ui::headless::InspectionNode;

const DESKTOP: (u32, u32) = (1280, 800);
const PHONE: (u32, u32) = (420, 860);

fn staged(viewport: (u32, u32)) -> TestApp<TlApp> {
    support::installed("tasks");
    let mut app =
        TestApp::<TlApp>::capture_at(Some("a busy week"), viewport).expect("the scenario stages");
    settle(&mut app);
    app
}

fn settle(app: &mut TestApp<TlApp>) {
    for _ in 0..8 {
        app.harness()
            .settle_for_capture(ui::time::Duration::from_millis(300));
        app.harness().relayout();
    }
}

fn walk<'a>(node: &'a InspectionNode, out: &mut Vec<&'a InspectionNode>) {
    out.push(node);
    for child in &node.children {
        walk(child, out);
    }
}

fn access_walk<'a>(node: &'a ui::a11y::AccessNode, out: &mut Vec<&'a ui::a11y::AccessNode>) {
    out.push(node);
    for child in &node.children {
        access_walk(child, out);
    }
}

/// Every accessible node of the window.
fn accessible(app: &mut TestApp<TlApp>) -> Vec<ui::a11y::AccessNode> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let mut out = Vec::new();
    for node in all {
        if let Some(access) = &node.semantics {
            let mut nodes = Vec::new();
            access_walk(access, &mut nodes);
            out.extend(nodes.into_iter().cloned());
        }
    }
    out
}

/// Where the accessible node named `name` (of `role`, when given) is drawn.
fn bounds_of(
    app: &mut TestApp<TlApp>,
    name: &str,
    role: Option<ui::a11y::Role>,
) -> Option<ui::kurbo::Rect> {
    accessible(app)
        .into_iter()
        .rev()
        .filter(|node| node.name.as_deref() == Some(name))
        .filter(|node| role.is_none_or(|role| node.role == role))
        .find_map(|node| node.bounds)
        .map(|b| ui::kurbo::Rect::new(b.x0, b.y0, b.x1, b.y1))
}

/// Every accessible name in the window, for a failure's message.
fn all_names(app: &mut TestApp<TlApp>) -> Vec<String> {
    accessible(app)
        .into_iter()
        .filter_map(|node| node.name)
        .collect()
}

/// Whether some node in the window says `text`.
fn reads(app: &mut TestApp<TlApp>, text: &str) -> bool {
    accessible(app).iter().any(|node| {
        node.name.as_deref().is_some_and(|name| name.contains(text))
            || node
                .value
                .as_deref()
                .is_some_and(|value| value.contains(text))
    })
}

/// The open menu's items, in order.
fn menu_items(app: &mut TestApp<TlApp>) -> Vec<String> {
    accessible(app)
        .into_iter()
        .filter(|node| node.role == ui::a11y::Role::MenuItem)
        .filter_map(|node| node.name)
        .collect()
}

/// The form's submit button, by its label: whether it can be pressed.
fn submit(app: &mut TestApp<TlApp>, label: &str) -> Option<((), bool)> {
    accessible(app)
        .into_iter()
        .rev()
        .find(|node| node.role == ui::a11y::Role::Button && node.name.as_deref() == Some(label))
        .map(|node| ((), !node.state.disabled))
}

/// Close the form the way a person does from the keyboard: Escape is its Cancel.
fn escape(app: &mut TestApp<TlApp>) {
    app.harness().key("Escape");
    settle(app);
}

fn right_click(app: &mut TestApp<TlApp>, at: ui::kurbo::Point) {
    let harness = app.harness();
    harness.send(&ui::synthetic_input::cursor_moved(at.x, at.y));
    harness.send(&ui::synthetic_input::mouse_input(
        ui::winit::event::ElementState::Pressed,
        ui::winit::event::MouseButton::Right,
    ));
    harness.send(&ui::synthetic_input::mouse_input(
        ui::winit::event::ElementState::Released,
        ui::winit::event::MouseButton::Right,
    ));
    harness.relayout();
}

fn click(app: &mut TestApp<TlApp>, at: ui::kurbo::Point) {
    app.harness().click_at(at.x, at.y);
    settle(app);
}

/// Open the menu of the task `title` and choose `item`.
fn choose(app: &mut TestApp<TlApp>, title: &str, item: &str) {
    let row = bounds_of(app, title, None)
        .unwrap_or_else(|| panic!("the task {title} shows: {:?}", all_names(app)));
    right_click(app, row.center());
    let items = menu_items(app);
    let at = bounds_of(app, item, Some(ui::a11y::Role::MenuItem))
        .unwrap_or_else(|| panic!("the menu offers {item}: {items:?}"));
    click(app, at.center());
}

/// The form's field named `label`, which has the keyboard: a form opens on its first field.
fn field(app: &mut TestApp<TlApp>, label: &str) {
    let focused = accessible(app).into_iter().any(|node| {
        node.role == ui::a11y::Role::TextInput
            && node.name.as_deref() == Some(label)
            && node.state.focused
    });
    assert!(
        focused,
        "the form has its field {label}, focused: {:?}",
        all_names(app)
    );
}

#[test]
fn a_tasks_menu_offers_the_actions_that_ask_for_more() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    let row = bounds_of(&mut app, "Book a haircut", None).expect("the task shows");
    right_click(&mut app, row.center());
    let items = menu_items(&mut app);
    for item in ["Snooze until…", "Assign to…"] {
        assert!(items.iter().any(|shown| shown == item), "{item}: {items:?}");
    }
}

#[test]
fn snoozing_a_task_asks_until_when_and_runs_once_the_day_reads() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    choose(&mut app, "Book a haircut", "Snooze until…");
    assert!(
        reads(&mut app, "Snooze until…"),
        "the form is titled by its action: {:?}",
        all_names(&mut app)
    );
    let Some(((), enabled)) = submit(&mut app, "Snooze until") else {
        let nodes: Vec<String> = accessible(&mut app)
            .into_iter()
            .map(|node| format!("{:?} {:?} {:?}", node.role, node.name, node.state.disabled))
            .collect();
        panic!("the form has its submit: {nodes:#?}");
    };
    assert!(!enabled, "nothing typed yet: submit waits");

    field(&mut app, "Until");
    app.harness().type_text("not a day");
    settle(&mut app);
    let ((), enabled) = submit(&mut app, "Snooze until").unwrap();
    assert!(
        !enabled,
        "what does not read as a day keeps submit disabled"
    );
    assert!(reads(&mut app, "Not a day"), "{:?}", all_names(&mut app));

    // Closing the form keeps what was typed for next time.
    escape(&mut app);
    assert!(
        submit(&mut app, "Snooze until").is_none(),
        "the form closed"
    );
    choose(&mut app, "Book a haircut", "Snooze until…");
    let shown = accessible(&mut app)
        .into_iter()
        .find(|node| {
            node.role == ui::a11y::Role::TextInput && node.name.as_deref() == Some("Until")
        })
        .and_then(|node| node.value);
    assert_eq!(
        shown.as_deref(),
        Some("not a day"),
        "the lens kept the typing"
    );

    field(&mut app, "Until");
    // Its caret is at the end of what it kept.
    app.harness().key("End");
    for _ in "not a day".chars() {
        app.harness().key("Backspace");
    }
    app.harness().type_text("2026-07-20");
    settle(&mut app);
    let ((), enabled) = submit(&mut app, "Snooze until").unwrap();
    assert!(enabled, "the day reads: {:?}", all_names(&mut app));
    // Enter is the form's submit.
    app.harness().key("Enter");
    settle(&mut app);
    assert!(
        submit(&mut app, "Snooze until").is_none(),
        "submit closed the form"
    );
    assert!(
        reads(&mut app, "Jul 20"),
        "the task is due on the day it was snoozed until: {:?}",
        all_names(&mut app)
    );
}

#[test]
fn a_button_on_the_task_page_asks_whom_to_assign_it_to() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    let row = bounds_of(&mut app, "Book a haircut", None).expect("the task shows");
    click(&mut app, row.center());
    let button = bounds_of(&mut app, "Assign to…", Some(ui::a11y::Role::Button))
        .unwrap_or_else(|| panic!("the page offers Assign to…: {:?}", all_names(&mut app)));
    click(&mut app, button.center());
    let names = all_names(&mut app);
    assert!(
        accessible(&mut app)
            .iter()
            .any(|node| node.role == ui::a11y::Role::Dialog
                && node.name.as_deref() == Some("Assign to…")),
        "the button opened the action's form: {names:?}"
    );
    assert!(names.iter().any(|name| name == "To"), "{names:?}");
}

#[test]
fn shots() {
    let Some(dir) = std::env::var_os("TILO_FORMS_SHOTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _settled = ui::motion::testing::settled();
    for (size, viewport) in [("desktop", DESKTOP), ("phone", PHONE)] {
        for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
            let mut app = staged(viewport);
            app.harness().set_theme(theme);
            settle(&mut app);
            let name = |state: &str| dir.join(format!("{state}-{size}-{shade}.png"));
            if size == "phone" {
                // The inbox's list is the window on a phone; its rows are the tasks.
                settle(&mut app);
            }
            choose(&mut app, "Book a haircut", "Snooze until…");
            app.harness().render_png(&name("snooze"));
            field(&mut app, "Until");
            app.harness().type_text("next friday");
            settle(&mut app);
            app.harness().render_png(&name("snooze-typed"));
            escape(&mut app);
            choose(&mut app, "Book a haircut", "Assign to…");
            app.harness().render_png(&name("assign"));
        }
    }
}
