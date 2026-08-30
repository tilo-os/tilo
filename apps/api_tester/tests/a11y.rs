//! Accessibility + keyboard proof for the retained API-tester tree.
//!
//! These tests drive the **real widget tree** through `ui::headless::Driver`
//! — the same event dispatch, layout, and `Widget::accessibility()` the live
//! app uses, with no display — and assert on the accessibility nodes the
//! AccessKit bridge exports and on keyboard focus traversal. They are the
//! reusable pattern every converted app should copy: build a `Driver`, assert
//! the focusable controls exist with the right roles/labels, Tab through them,
//! and activate one with Enter to prove the real handler runs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use api_tester::ApiTesterApp;
use ui::State;
use ui::a11y::Role;
use ui::headless::Driver;

fn driver() -> Driver {
    // The demo build populates collections / environments / history / a
    // response so every control class is present.
    Driver::new(
        State::new(),
        Box::new(ApiTesterApp::new_demo()),
        (1280, 800),
    )
}

/// Every interactive control is a real focusable widget with an accessibility
/// node carrying the correct role, name, and value — the thing a single
/// painted canvas could never provide.
#[test]
fn every_control_is_a_focusable_accessible_node() {
    let d = driver();
    let nodes = d.focus_access_nodes();
    let roles: Vec<Role> = nodes.iter().map(|n| n.role).collect();

    // Method + environment pickers are combo boxes.
    assert!(
        roles.iter().filter(|r| **r == Role::ComboBox).count() >= 2,
        "method + environment pickers are ComboBoxes, got {roles:?}"
    );
    // The URL editor is a real text field.
    assert!(
        roles.contains(&Role::TextInput),
        "URL editor is a TextInput: {roles:?}"
    );
    // The request tabs + the response-view switch are segmented groups.
    assert!(
        roles.iter().filter(|r| **r == Role::Group).count() >= 2,
        "request tabs + response view are Groups, got {roles:?}"
    );
    // Send + every sidebar/history row are buttons.
    assert!(
        roles.contains(&Role::Button),
        "Send + rows are Buttons: {roles:?}"
    );

    // Labels / values a screen reader announces.
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Button && n.name.as_deref() == Some("Send")),
        "the Send button carries its label"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::ComboBox && n.value.as_deref() == Some("GET")),
        "the method picker announces the selected verb"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::ComboBox && n.value.as_deref() == Some("Production")),
        "the environment picker announces the active environment"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Group && n.value.as_deref() == Some("Params")),
        "the request-tab group announces the active tab"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Group && n.value.as_deref() == Some("Pretty")),
        "the response-view group announces the active view"
    );
    // A saved-request row is an addressable, labelled button.
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Button && n.name.as_deref() == Some("GET List users")),
        "a saved request is a focusable, labelled row"
    );
    // A collection header exposes its expanded state.
    let coll = nodes
        .iter()
        .find(|n| n.name.as_deref() == Some("Users API, collection, 4 requests"))
        .expect("the collection header is an accessible row");
    assert_eq!(
        coll.state.expanded,
        Some(true),
        "an expanded collection reports it"
    );
}

/// Tab / Shift+Tab move keyboard focus between the real controls in tree
/// order — the traversal the compositor's `App` performs, exercised here
/// through the same focus walk.
#[test]
fn tab_traverses_every_control_in_order() {
    let mut d = driver();
    let stops = d.tab_through();

    // The builder chrome comes first, top-left to top-right: method picker,
    // URL field, Send, then the request-tab group.
    let roles: Vec<Role> = stops.iter().map(|(r, _)| *r).collect();
    assert_eq!(
        roles.first(),
        Some(&Role::ComboBox),
        "first stop is the method picker"
    );
    assert_eq!(
        roles.get(1),
        Some(&Role::TextInput),
        "second stop is the URL field"
    );
    assert_eq!(roles.get(2), Some(&Role::Button), "third stop is Send");
    assert_eq!(stops[2].1.as_deref(), Some("Send"));
    assert_eq!(
        roles.get(3),
        Some(&Role::Group),
        "fourth stop is the request tabs"
    );

    // Every focusable control is reached exactly once.
    assert_eq!(
        stops.len(),
        d.focus_order().len(),
        "Tab visits every focusable node"
    );

    // Focus reaches into the sidebar rows.
    assert!(
        stops
            .iter()
            .any(|(r, n)| *r == Role::Button && n.as_deref() == Some("GET List users")),
        "Tab reaches the saved-request rows"
    );

    // Shift+Tab reverses.
    d.set_modifiers(&["shift".into()]);
    let back = d.tab(true);
    assert!(back.is_some(), "Shift+Tab moves focus backward");
}

/// Enter on a focused row activates it through the widget's real handler —
/// the same path a screen reader's "activate" takes — and the effect lands in
/// the app (the collection collapses, hiding its request rows).
#[test]
fn enter_activates_a_focused_row_through_the_real_handler() {
    let mut d = driver();
    let target = "Users API, collection, 4 requests";

    // Tab until the collection header holds focus.
    let mut reached = false;
    for _ in 0..=d.focus_order().len() {
        d.tab(false);
        if d.focused_access_node().and_then(|n| n.name).as_deref() == Some(target) {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab can land on the collection header");

    // Expanded: its request rows are present.
    let before = d.focus_access_nodes();
    assert!(
        before
            .iter()
            .any(|n| n.name.as_deref() == Some("GET List users")),
        "before activation the collection is expanded"
    );

    // Activate with the keyboard.
    d.key("Enter");

    // Collapsed: the request rows are gone and the header reports it.
    let after = d.focus_access_nodes();
    assert!(
        !after
            .iter()
            .any(|n| n.name.as_deref() == Some("GET List users")),
        "Enter collapsed the collection, hiding its request rows"
    );
    let coll = after
        .iter()
        .find(|n| n.name.as_deref() == Some(target))
        .expect("the collection header is still present");
    assert_eq!(
        coll.state.expanded,
        Some(false),
        "the header now reports collapsed"
    );
}
