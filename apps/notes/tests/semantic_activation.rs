//! Pressing a control by the route the page's semantics mirror names it
//! performs what a press on it does, for the controls the window does not lay
//! out as widgets of its own: an item of the outline (a synthetic child of the
//! list, shown under the widget that wraps the list as well as under the
//! list) and a note's row in the sidebar (a widget named by its own node,
//! whose list keeps what opening it means).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fmt::Write as _;

use crate::framework_support::{add_note, notes as notebook, surface, with_note};
use notes::{MainPanel, NotesShell};
use ui::framework::testing::TestApp;
use ui::theme::Theme;
use ui::{
    a11y::{AccessNode, Action},
    headless::{FrameHarness, InspectionNode},
};

/// Every (owner path, action target) the inspection reaches for a node
/// named `name`, in the order the page's semantics mirror walks them: the
/// widget the node stands under, and the target an action on it runs.
fn routes(node: &InspectionNode, name: &str, found: &mut Vec<(String, Option<String>)>) {
    fn visit(
        access: &AccessNode,
        owner: &str,
        name: &str,
        found: &mut Vec<(String, Option<String>)>,
    ) {
        if access.name.as_deref() == Some(name) {
            found.push((owner.into(), access.action_target.clone()));
        }
        for child in &access.children {
            visit(child, owner, name, found);
        }
    }
    if let Some(access) = &node.semantics {
        visit(access, &node.path, name, found);
    }
    for child in &node.children {
        routes(child, name, found);
    }
}

fn named(driver: &FrameHarness, name: &str) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    routes(&driver.inspection().root, name, &mut found);
    found
}

/// Press the control `name` reads as, where the page's mirror finds it: its
/// first route, the widget the walk shows it under.
fn press_by_name(driver: &mut FrameHarness, name: &str, action: &Action) -> Result<(), String> {
    let routes = named(driver, name);
    let Some(route) = routes.first() else {
        return Err(format!("no control named {name:?}"));
    };
    driver.perform_semantic_named(&route.0, route.1.as_deref(), action)
}

/// Read or drive the note panel.
fn panel<T>(app: &mut TestApp<NotesShell>, visit: impl FnOnce(&mut MainPanel) -> T) -> T {
    surface(app, |notes| visit(notes.main_panel_mut()))
}

/// A note whose headings stand far enough apart that revealing one scrolls
/// the body to it.
fn long_note() -> String {
    let mut body = String::from("# Long page\n\n");
    for i in 0..40 {
        let _ = writeln!(body, "Paragraph number {i} with some words.\n");
    }
    body.push_str("## Deep section\n\n");
    for i in 0..40 {
        let _ = writeln!(body, "Later paragraph {i} with some words.\n");
    }
    body
}

/// A heading in the outline is a synthetic child of the list, shown under
/// the sidebar that wraps the list as well as under the list itself. Pressed
/// by name, it reveals its heading in the body, exactly as a press on it
/// does.
#[test]
fn an_outline_heading_pressed_by_name_reveals_it_in_the_note() {
    let (mut app, _page) = with_note("Long page", &long_note());
    app.harness()
        .relayout_themed((1000, 400), Theme::dark())
        .unwrap();
    app.settle();
    let routes = named(app.harness(), "Deep section");
    assert!(
        routes.len() >= 2,
        "the sidebar and the list it holds both show the heading: {routes:?}"
    );
    assert!(
        !routes[0].0.contains("NavList"),
        "the walk reaches the heading under the wrapping sidebar first: {routes:?}"
    );
    assert_eq!(
        panel(&mut app, |panel| panel.scroll_y_for_test()),
        0.0,
        "the note starts at its top"
    );
    press_by_name(app.harness(), "Deep section", &Action::Activate)
        .expect("a press on the heading's row reaches it");
    app.harness().relayout_at((1000, 400)).unwrap();
    assert!(
        panel(&mut app, |panel| panel.scroll_y_for_test()) > 0.0,
        "the heading's row reveals the heading in the body"
    );
}

/// The same press on the route the inner list publishes, which was the one
/// route that worked: both paths reach the one row.
#[test]
fn the_lists_own_route_reaches_the_same_row() {
    let (mut app, _page) = with_note("Long page", &long_note());
    app.harness()
        .relayout_themed((1000, 400), Theme::dark())
        .unwrap();
    app.settle();
    let route = named(app.harness(), "Deep section")
        .into_iter()
        .find(|route| route.0.contains("NavList"))
        .expect("the list publishes the heading too");
    app.harness()
        .perform_semantic_named(&route.0, route.1.as_deref(), &Action::Activate)
        .expect("the row runs");
    app.harness().relayout_at((1000, 400)).unwrap();
    assert!(panel(&mut app, |panel| panel.scroll_y_for_test()) > 0.0);
}

/// A note's row in the sidebar names itself as a control, and its list keeps
/// what opening it means. Pressed by name, the row opens the note, as a
/// pointer on it does.
#[test]
fn a_note_rows_own_name_opens_the_note() {
    let mut app = notebook();
    let plan = add_note(&mut app, "Plan", "");
    let groceries = add_note(&mut app, "Groceries", "");
    app.settle();
    assert_eq!(
        surface(&mut app, |notes| notes.selected_for_test()),
        Some(plan),
        "the first note is the one showing"
    );
    let route = named(app.harness(), "Groceries")
        .into_iter()
        .find(|route| route.0.contains("ListRow"))
        .expect("the note's own row names it");
    app.harness()
        .perform_semantic_named(&route.0, route.1.as_deref(), &Action::Activate)
        .expect("the row is pressed where it stands");
    app.harness().relayout();
    app.settle();
    assert_eq!(
        surface(&mut app, |notes| notes.selected_for_test()),
        Some(groceries),
        "the row opens its note"
    );
}
