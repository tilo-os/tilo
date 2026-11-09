//! An action waiting for its arguments opens its form (forms-and-diagrams.md §1), in Tasks as a
//! person uses it: a task's menu offers "Pick date…" and "Assign…", whose parameters nobody
//! gave; choosing one shows its form over the window, a day that does not read keeps the submit
//! disabled, what was typed survives closing the form, and submit runs the action once.
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
    // The scenario answers with a task, shown open where it stands. Choosing a place leaves it,
    // as a person does, so what the test reads next is the list.
    open_place(&mut app, "Inbox");
    app
}

fn settle(app: &mut TestApp<TlApp>) {
    for _ in 0..8 {
        app.harness()
            .settle_for_capture(ui::time::Duration::from_millis(300));
        app.harness().relayout();
    }
}

/// Chooses the sidebar's place `label`, as a person does: what is due before today waits in
/// Today, and the inbox holds only what no day and no project sorts.
fn open_place(app: &mut TestApp<TlApp>, label: &str) {
    let bounds = accessible(app)
        .into_iter()
        .find(|node| {
            node.role == ui::a11y::Role::TreeItem
                && node
                    .name
                    .as_deref()
                    .is_some_and(|name| name.starts_with(label))
        })
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("the sidebar offers {label}: {:?}", all_names(app)));
    app.harness().click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    settle(app);
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
    open_place(&mut app, "Today");
    let row = bounds_of(&mut app, "Book a haircut", None).expect("the task shows");
    right_click(&mut app, row.center());
    let items = menu_items(&mut app);
    // The day and the assignment both ask for what nobody gave them; what the view places
    // stands on its own, and the commands the task's type offers wait behind "More".
    for item in ["Pick date…", "Assign…"] {
        assert!(items.iter().any(|shown| shown == item), "{item}: {items:?}");
    }
}

#[test]
fn picking_a_day_asks_until_when_and_runs_once_the_day_reads() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    open_place(&mut app, "Today");
    choose(&mut app, "Book a haircut", "Pick date…");
    assert!(
        reads(&mut app, "Pick date…"),
        "the form is titled by its action: {:?}",
        all_names(&mut app)
    );
    let Some(((), enabled)) = submit(&mut app, "Pick date") else {
        let nodes: Vec<String> = accessible(&mut app)
            .into_iter()
            .map(|node| format!("{:?} {:?} {:?}", node.role, node.name, node.state.disabled))
            .collect();
        panic!("the form has its submit: {nodes:#?}");
    };
    // A day may be none, which clears the task's: an empty form submits, and only a day that
    // does not read waits.
    assert!(
        enabled,
        "an empty day submits as none: {:?}",
        all_names(&mut app)
    );

    field(&mut app, "Due");
    app.harness().type_text("not a day");
    settle(&mut app);
    let ((), enabled) = submit(&mut app, "Pick date").unwrap();
    assert!(
        !enabled,
        "what does not read as a day keeps submit disabled"
    );
    assert!(reads(&mut app, "Not a day"), "{:?}", all_names(&mut app));

    // Closing the form keeps what was typed for next time.
    escape(&mut app);
    assert!(submit(&mut app, "Pick date").is_none(), "the form closed");
    choose(&mut app, "Book a haircut", "Pick date…");
    let shown = accessible(&mut app)
        .into_iter()
        .find(|node| node.role == ui::a11y::Role::TextInput && node.name.as_deref() == Some("Due"))
        .and_then(|node| node.value);
    assert_eq!(
        shown.as_deref(),
        Some("not a day"),
        "the view state kept the typing"
    );

    field(&mut app, "Due");
    // Its caret is at the end of what it kept.
    app.harness().key("End");
    for _ in "not a day".chars() {
        app.harness().key("Backspace");
    }
    app.harness().type_text("2026-07-20");
    settle(&mut app);
    let ((), enabled) = submit(&mut app, "Pick date").unwrap();
    assert!(enabled, "the day reads: {:?}", all_names(&mut app));
    // Enter is the form's submit.
    app.harness().key("Enter");
    settle(&mut app);
    assert!(
        submit(&mut app, "Pick date").is_none(),
        "submit closed the form"
    );
    if !reads(&mut app, "Jul 20") {
        let today: Vec<String> = all_names(&mut app);
        open_place(&mut app, "Inbox");
        let inbox: Vec<String> = all_names(&mut app);
        open_place(&mut app, "Upcoming");
        let upcoming: Vec<String> = all_names(&mut app);
        panic!("NOT CHANGED\ntoday: {today:?}\ninbox: {inbox:?}\nupcoming: {upcoming:?}");
    }
}

#[test]
fn the_task_pages_assignee_opens_the_people_to_pick_from() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    open_place(&mut app, "Today");
    let row = bounds_of(&mut app, "Book a haircut", None).expect("the task shows");
    click(&mut app, row.center());
    // The page keeps one person field for who does the task, showing nobody yet.
    let chip = bounds_of(&mut app, "No one", Some(ui::a11y::Role::Button)).unwrap_or_else(|| {
        panic!(
            "the page offers who does the task: {:?}",
            all_names(&mut app)
        )
    });
    click(&mut app, chip.center());
    let names = all_names(&mut app);
    assert!(
        names.iter().any(|name| name == "Me"),
        "the field opened the people to pick from: {names:?}"
    );
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
            choose(&mut app, "Book a haircut", "Pick date…");
            app.harness().render_png(&name("snooze"));
            field(&mut app, "Due");
            app.harness().type_text("next friday");
            settle(&mut app);
            app.harness().render_png(&name("snooze-typed"));
            escape(&mut app);
            choose(&mut app, "Book a haircut", "Assign…");
            app.harness().render_png(&name("assign"));
        }
    }
}
