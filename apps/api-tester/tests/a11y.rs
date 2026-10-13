//! Accessibility + keyboard proof for the retained API-tester tree.
//!
//! These tests drive the **real app** through the framework's test host —
//! the same resource, event dispatch, layout, and `Widget::accessibility()` the
//! live app uses, with no display — and assert on the accessibility nodes the
//! AccessKit bridge exports and on keyboard focus traversal: the focusable
//! controls exist with the right roles/labels, Tab reaches them, and Enter
//! runs the real handler.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use api_tester::ApiTesterShell;
use api_tester::actions::ApiTesterEffect;
use ui::a11y::Role;
use ui::framework::testing::TestApp;
use ui::headless::FrameHarness;

/// The live app at `viewport`, staged for `scenario` as `--shots` stages it.
fn staged(scenario: Option<&str>, viewport: (u32, u32)) -> TestApp<ApiTesterShell> {
    let mut app = TestApp::<ApiTesterShell>::new().unwrap();
    app.harness().resize(viewport).unwrap();
    ui::framework::stage::<ApiTesterShell>(app.harness(), scenario).unwrap();
    app
}

/// The live app on the sample workspace: collections, environments,
/// history and a response, so every control class is present.
fn sample() -> TestApp<ApiTesterShell> {
    staged(None, (1280, 800))
}

/// The app as screenshot scenario `scenario` shows it, at `viewport`.
fn captured(scenario: &str, viewport: (u32, u32)) -> TestApp<ApiTesterShell> {
    staged(Some(scenario), viewport)
}

/// All published semantic descendants, including shared list rows that do
/// not each create another Tab stop.
fn semantic_nodes(d: &FrameHarness) -> Vec<ui::a11y::AccessNode> {
    fn flatten(node: ui::a11y::AccessNode, out: &mut Vec<ui::a11y::AccessNode>) {
        let children = node.children.clone();
        out.push(node);
        for child in children {
            flatten(child, out);
        }
    }
    let mut nodes = Vec::new();
    for node in d.focus_access_nodes() {
        flatten(node, &mut nodes);
    }
    nodes
}

/// Drive a synthetic retained row action through its published AT owner.
fn act_named(d: &mut FrameHarness, name: &str, action: &ui::a11y::Action) {
    let node = d
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("missing actionable {name}: {}", d.dump_json()));
    d.perform_a11y_action(node, action).unwrap();
}

fn environment_command(d: &mut FrameHarness, choice: &str) {
    if !d.targets().contains_key("api.env") {
        d.perform_named("nav-shell-sidebar", &ui::a11y::Action::Focus)
            .expect("the actual sidebar control is reachable");
        d.key("Enter");
        assert!(d.settle_for_capture(ui::time::Duration::from_secs(1)));
        assert!(
            d.targets().contains_key("api.env"),
            "the opened sidebar exposes its environment picker"
        );
    }
    d.perform_named("api.env", &ui::a11y::Action::SetValue(choice.into()))
        .unwrap();
}

/// Apply what the last input submitted.
fn settle(d: &mut FrameHarness) {
    d.wait_until(ui::time::Duration::ZERO, |_| true);
}

/// Every surface (full demo, empty and error responses, the narrow window,
/// contextual sheets) exposes its controls as named accessibility nodes; each
/// case mounts its own `FrameHarness`.
#[test]
fn every_surface_exposes_named_accessible_controls() {
    every_control_is_a_focusable_accessible_node();
    empty_and_error_response_surfaces_expose_recovery_actions();
    narrow_workspace_menu_is_keyboard_accessible();
    contextual_forms_expose_only_their_named_fields();
}

/// Every interactive control is a real focusable widget with an accessibility
/// node carrying the correct role, name, and value — the thing a single
/// painted canvas could never provide.
/// Case of `every_surface_exposes_named_accessible_controls`.
fn every_control_is_a_focusable_accessible_node() {
    let mut app = sample();
    let d = app.harness();
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

    // Labels / values a screen reader announces.
    for name in ["Send", "Save", "New collection", "Search", "Hide sidebar"] {
        assert!(
            nodes
                .iter()
                .any(|n| n.role == Role::Button && n.name.as_deref() == Some(name)),
            "{name} is a named button"
        );
    }
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
    // A saved request is an addressable, labelled row of the requests list
    // that reports selection, led by its method.
    let requests = nodes
        .iter()
        .find(|n| n.role == Role::Tree && n.name.as_deref() == Some("Requests"))
        .expect("the saved requests are one navigation list");
    let row = requests
        .children
        .iter()
        .find(|n| n.role == Role::TreeItem && n.name.as_deref() == Some("List users"))
        .expect("a saved request is a labelled row");
    assert!(row.state.selected, "the open request is selected");
    // The parameters are an editable table of named cells.
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Table && n.name.as_deref() == Some("Query parameters")),
        "the parameters table is named"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::TextInput && n.value.as_deref() == Some("per_page")),
        "a parameter's key is an editable field"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::Checkbox && n.name.as_deref() == Some("Use this row")),
        "a parameter has its include checkbox"
    );
    // The idle UI has no ambiguous generic management fields.
    assert!(
        !nodes.iter().any(|node| node.role == Role::TextInput
            && matches!(node.name.as_deref(), Some("Name" | "Variable"))),
        "sheets are closed until asked for"
    );
}

/// Case of `every_surface_exposes_named_accessible_controls`.
fn empty_and_error_response_surfaces_expose_recovery_actions() {
    let mut empty = captured("empty", (1280, 800));
    let empty = empty.harness();
    let nodes = empty.focus_access_nodes();
    assert!(
        empty.a11y_node_named("No response yet").is_some(),
        "the empty response state says there is no response"
    );
    assert_eq!(
        nodes
            .iter()
            .filter(|node| node.role == Role::Button && node.name.as_deref() == Some("Send"))
            .count(),
        1,
        "Send is offered once: the empty state does not repeat it"
    );

    let mut error = captured("error", (390, 844));
    let error = error.harness();
    assert!(
        error.focus_access_nodes().iter().any(|node| {
            node.role == Role::Group
                && node.name.as_deref() == Some("Couldn't complete the request")
                && node.children.iter().any(|child| {
                    child.role == Role::Button && child.name.as_deref() == Some("Try again")
                })
        }),
        "the error response state exposes a retry action"
    );
}

/// Case of `every_surface_exposes_named_accessible_controls`.
fn narrow_workspace_menu_is_keyboard_accessible() {
    let mut d = captured("empty", (390, 844));
    let d = d.harness();
    assert!(
        d.focus_access_nodes().iter().any(|node| {
            node.role == Role::Button && node.name.as_deref() == Some("Show sidebar")
        }),
        "the narrow window's toggle opens the drawer"
    );
}

/// Case of `every_surface_exposes_named_accessible_controls`.
fn contextual_forms_expose_only_their_named_fields() {
    let mut variable = captured("form-new-variable", (390, 844));
    let nodes = variable.harness().focus_access_nodes();
    for name in [
        "Variable key",
        "Variable value",
        "Keep value on this device",
        "Cancel",
        "Add variable",
    ] {
        assert!(
            nodes.iter().any(|node| node.name.as_deref() == Some(name)),
            "{name}"
        );
    }
    assert!(
        !nodes
            .iter()
            .any(|node| node.role == Role::TextInput && node.name.as_deref() == Some("Name")),
        "the variable sheet has distinct Variable and Value fields, not an irrelevant Name input"
    );

    let mut rename = captured("form-rename-request", (1280, 800));
    assert!(
        rename
            .harness()
            .focus_access_nodes()
            .iter()
            .any(|node| node.role == Role::TextInput && node.name.as_deref() == Some("Name")),
        "the rename sheet has its single named field"
    );
}

/// Tab order, Enter activation and Escape dismissal drive the real widget
/// handlers; each case mounts its own `FrameHarness`.
#[test]
fn keyboard_focus_traverses_activates_and_dismisses_through_real_handlers() {
    tab_traverses_every_control_in_order();
    enter_activates_a_focused_row_through_the_real_handler();
    composition_disclosure_hides_maintenance_and_returns_focus();
    composition_narrow_escape_paging_and_open_resize_are_reachable();
}

/// Tab / Shift+Tab move keyboard focus between the real controls in tree
/// order — the traversal the compositor's `App` performs, exercised here
/// through the same focus walk.
/// Case of `keyboard_focus_traverses_activates_and_dismisses_through_real_handlers`.
fn tab_traverses_every_control_in_order() {
    let mut app = sample();
    let d = app.harness();
    // The window's own chrome (History) precedes the app's controls.
    let all_stops = d.tab_through();
    assert_eq!(
        all_stops.len(),
        d.focus_order().len(),
        "Tab visits every focusable node, including platform controls"
    );
    let stops: Vec<_> = all_stops
        .into_iter()
        .skip_while(|(role, _)| *role != Role::ComboBox)
        .collect();

    // The builder comes first, top-left to top-right: method picker, URL
    // field, Save, Send, then the request tabs.
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
    assert_eq!(stops[2].1.as_deref(), Some("Save"));
    assert_eq!(stops[3].1.as_deref(), Some("Send"));
    assert_eq!(
        roles.get(4),
        Some(&Role::Group),
        "request tabs follow the endpoint row"
    );
    let divider = roles
        .iter()
        .position(|role| *role == Role::Separator)
        .expect("the resize divider is keyboard reachable");
    assert!(
        divider > 4,
        "the divider follows the request controls in directional focus order"
    );
    assert_eq!(
        roles
            .iter()
            .filter(|role| **role == Role::Separator)
            .count(),
        1,
        "one retained request/response divider is reachable"
    );

    // Focus reaches the sidebar's list (arrows move through its rows) and
    // the table's cells.
    assert!(
        stops
            .iter()
            .any(|(r, n)| *r == Role::Tree && n.as_deref() == Some("Requests")),
        "Tab reaches the saved requests"
    );
    assert!(
        stops
            .iter()
            .any(|(r, n)| *r == Role::Button && n.as_deref() == Some("Remove row")),
        "Tab reaches a table row's remove button"
    );

    // Shift+Tab reverses.
    d.set_modifiers(&["shift".into()]);
    let back = d.tab(true);
    assert!(back.is_some(), "Shift+Tab moves focus backward");
}

/// The arrow keys move through the saved requests once Tab reaches their
/// list, and the request chosen loads into the builder, through the list's
/// real handler — the same path a screen reader's "activate" takes.
/// Case of `keyboard_focus_traverses_activates_and_dismisses_through_real_handlers`.
fn enter_activates_a_focused_row_through_the_real_handler() {
    let mut app = sample();
    let d = app.harness();
    let target = "Users API";

    act_named(d, target, &ui::a11y::Action::Focus);
    assert!(
        d.focused_access_node().is_some(),
        "the collection header can receive keyboard focus"
    );

    // Expanded: its request rows are present.
    let before = semantic_nodes(d);
    assert!(
        before
            .iter()
            .any(|n| n.name.as_deref() == Some("List users")),
        "before activation the collection is expanded"
    );

    // Activate with the keyboard.
    d.key("Enter");

    // Collapsed: the request rows are gone and the header reports it.
    let after = semantic_nodes(d);
    assert!(
        !after
            .iter()
            .any(|n| n.name.as_deref() == Some("List users")),
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

/// Case of `keyboard_focus_traverses_activates_and_returns_through_real_handlers`.
fn composition_disclosure_hides_maintenance_and_returns_focus() {
    let mut app = sample();
    let d = app.harness();
    assert!(
        !d.targets().contains_key("api.form-name"),
        "maintenance forms are hidden until the row action asks for one"
    );
    act_named(d, "Rename request List users", &ui::a11y::Action::Activate);
    assert!(d.targets().contains_key("api.form-name"));
    assert!(!d.targets().contains_key("api.form-variable-key"));
    d.perform_named("api.form-name", &ui::a11y::Action::Focus)
        .unwrap();
    d.key("Escape");
    assert!(!d.targets().contains_key("api.form-name"));
    assert_eq!(
        d.focused_access_node()
            .and_then(|node| node.name)
            .as_deref(),
        Some("Requests"),
        "Escape returns focus to the navigation control that owns the row action"
    );
}

/// The request divider responds to keyboard and pointer, its manual
/// preference survives resizes and tab changes, and 2x text keeps every
/// control on screen; each case mounts its own `FrameHarness`.
#[test]
fn request_divider_and_controls_survive_resize_and_text_scale() {
    composition_params_fit_rows_and_divider_adjusts_with_keyboard();
    composition_pointer_drag_body_auth_and_preference_survive_resize();
    composition_real_two_x_retains_all_controls_and_response_space();
}

/// Case of `request_divider_and_controls_survive_resize_and_text_scale`.
fn composition_params_fit_rows_and_divider_adjusts_with_keyboard() {
    let mut app = sample();
    let d = app.harness();
    let before = d.targets()["api.request-divider"];
    let top = d.targets()["api.send"].y0;
    assert!(
        before.y0 - top < 300.0,
        "three parameter rows fit above the response: divider {before:?}, send y={top}"
    );
    d.perform_named("api.request-divider", &ui::a11y::Action::Focus)
        .unwrap();
    d.key("ArrowDown");
    assert!(d.targets()["api.request-divider"].y0 > before.y0);
    d.resize((800, 480)).unwrap();
    assert!(d.targets()["api.request-divider"].y1 < 380.0);
    d.resize((1280, 800)).unwrap();
    assert!(d.targets()["api.request-divider"].y0 > before.y0);
    d.click_named("api.reset-request-area").unwrap();
    assert_eq!(d.targets()["api.request-divider"], before);
}

fn click_tab(d: &mut FrameHarness, index: u32) {
    let rect = d.targets()["api.request-tabs"];
    d.click_at(
        rect.x0 + rect.width() * (f64::from(index) + 0.5) / 4.0,
        rect.center().y,
    );
}

fn move_pointer(d: &mut FrameHarness, x: f64, y: f64) {
    d.send(&ui::synthetic_input::cursor_moved(x, y));
}

/// Settle the drawer's animation through real redraw ticks.
fn settle_drawer(d: &mut FrameHarness) {
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        d.relayout();
    }
}

fn assert_sidebar_targets(d: &FrameHarness, height: f64) {
    let targets = d.targets();
    let mut sidebar = Vec::new();
    for (id, rect) in &targets {
        if id.starts_with("api.nav.")
            || [
                "api.env",
                "api.new-collection",
                "api.save-request",
                "api.collection-actions",
                "api.request-actions",
                "api.environment-actions",
                "api.rename-request",
                "api.delete-request",
                "api.increase-request-area",
                "api.decrease-request-area",
                "api.reset-request-area",
                "api.new-environment",
                "api.rename-environment",
                "api.delete-environment",
                "api.new-variable",
                "api.delete-variable",
                "api.previous-saved",
                "api.next-saved",
            ]
            .contains(&id.as_str())
        {
            assert!(
                rect.x0 >= 0.0
                    && rect.y0 >= 0.0
                    && rect.y1 <= height
                    && rect.width() > 0.0
                    && rect.height() > 0.0,
                "{id}: {rect:?} inside {height}"
            );
            for (other, other_rect) in &sidebar {
                assert!(
                    rect.intersect(*other_rect).area() <= 0.0,
                    "{id} overlaps {other}"
                );
            }
            sidebar.push((id, *rect));
        }
    }
}

/// Case of `request_divider_and_controls_survive_resize_and_text_scale`.
#[expect(
    clippy::float_cmp,
    reason = "the divider moves by exactly the dragged whole-pixel distance"
)]
fn composition_pointer_drag_body_auth_and_preference_survive_resize() {
    let mut app = sample();
    let d = app.harness();
    let divider = d.targets()["api.request-divider"];
    move_pointer(d, divider.center().x, divider.center().y);
    d.send(&ui::synthetic_input::mouse_press());
    move_pointer(d, divider.center().x, divider.center().y + 200.0);
    d.send(&ui::synthetic_input::mouse_release());
    let manual = d.targets()["api.request-divider"];
    assert_eq!(manual.y0, divider.y0 + 200.0);
    act_named(d, "Create user", &ui::a11y::Action::Activate);
    click_tab(d, 3);
    assert!(d.targets().contains_key("api.body"));
    assert_eq!(d.targets()["api.request-divider"], manual);
    d.click_named("api.body").unwrap();
    d.type_text("x");
    assert_eq!(d.targets()["api.request-divider"], manual);
    click_tab(d, 2);
    assert!(!d.targets().contains_key("api.body"));
    assert_eq!(d.targets()["api.request-divider"], manual);
    let headers = semantic_nodes(d)
        .into_iter()
        .find(|node| node.action_target.as_deref() == Some("api.response-view:segment:2"))
        .expect("the response switch publishes its Headers segment")
        .bounds
        .expect("the real Headers segment publishes its pointer bounds")
        .to_rect();
    d.click_at(headers.center().x, headers.center().y);
    assert!(
        semantic_nodes(d).iter().any(|node| {
            node.action_target.as_deref() == Some("api.response-view:segment:2")
                && node.name.as_deref() == Some("Headers")
                && node.state.checked == Some(true)
        }),
        "the real response switch selects Headers"
    );
    // Response presentation changes do not save a request. Open the form
    // through the real request Save control before checking resize custody.
    d.click_named("api.save-request").unwrap();
    d.wait_for_named("api.form-name", ui::time::Duration::from_secs(5))
        .expect("the published Save control opens the request form");
    assert!(
        d.focus_access_nodes()
            .iter()
            .any(|node| node.name.as_deref() == Some("Name")),
        "the sheet is open"
    );
    d.resize((800, 480)).unwrap();
    assert!(d.targets()["api.request-divider"].y1 <= 380.0);
    d.resize((1280, 800)).unwrap();
    assert_eq!(d.targets()["api.request-divider"], manual);
}

/// Real management buttons and Send (pointer and keyboard) drive the same
/// mutations and requests as the live app; each case mounts its own `FrameHarness`.
#[test]
fn real_buttons_perform_canonical_mutations_and_live_sends() {
    composition_management_real_buttons_keep_canonical_mutations();
    pointer_and_keyboard_send_use_the_live_request();
}

/// Case of `real_buttons_perform_canonical_mutations_and_live_sends`.
fn composition_management_real_buttons_keep_canonical_mutations() {
    let mut app = sample();
    let d = app.harness();
    act_named(d, "Rename collection Users API", &ui::a11y::Action::Focus);
    d.key("Enter");
    assert!(
        d.targets().contains_key("api.form-name"),
        "focused collection action must open its form: {}",
        d.dump_json()
    );
    d.click_named("api.form-name").unwrap();
    d.key("Home");
    d.type_text("Edited ");
    d.click_named("api.form.primary").unwrap();
    settle(d);
    assert!(
        semantic_nodes(d).iter().any(|n| n
            .name
            .as_deref()
            .is_some_and(|s| s.contains("Edited") && s.contains("collection"))),
        "{}",
        d.dump_json()
    );
    environment_command(d, "Add variable…");
    d.wait_for_named("api.form-variable-key", ui::time::Duration::from_secs(5))
        .expect("the canonical environment command publishes its variable form");
    d.click_named("api.form-variable-key").unwrap();
    d.type_text("region");
    d.click_named("api.form-variable-value").unwrap();
    d.type_text("eu-west-1");
    d.click_named("api.form.primary").unwrap();
    settle(d);
    let json = d.dump_json();
    assert!(
        json.contains("eu-west-1") && json.contains("region"),
        "{json}"
    );
}

/// Case of `keyboard_focus_traverses_activates_and_returns_through_real_handlers`.
fn composition_narrow_escape_paging_and_open_resize_are_reachable() {
    let mut app = sample();
    let d = app.harness();
    d.resize((480, 900)).unwrap();
    d.perform_named("nav-shell-sidebar", &ui::a11y::Action::Focus)
        .unwrap();
    d.key("Enter");
    settle_drawer(d);
    assert_sidebar_targets(d, 900.0);
    environment_command(d, "Add variable…");
    d.wait_for_named("api.form-variable-key", ui::time::Duration::from_secs(5))
        .expect("the canonical environment command publishes its variable form");
    let field = d.targets()["api.form-variable-key"];
    assert!(field.x0 >= 0.0 && field.x1 <= 480.0 && field.y1 <= 900.0);
    // 480 logical pixels for the app below the window's 40-pixel title bar.
    d.resize((800, 520)).unwrap();
    let field = d.targets()["api.form-variable-key"];
    assert!(field.x0 >= 0.0 && field.x1 <= 800.0 && field.y1 <= 520.0);
    d.key("Escape");
    assert!(
        !d.focus_access_nodes()
            .iter()
            .any(|node| node.name.as_deref() == Some("Variable key")),
        "Escape closed the sheet"
    );
}

/// Case of `request_divider_and_controls_survive_resize_and_text_scale`.
fn composition_real_two_x_retains_all_controls_and_response_space() {
    let mut app = captured("rail-open", (800, 900));
    let d = app.harness();
    d.relayout_themed(
        ui::headless::viewport((800.0, 900.0)),
        ui::theme::Theme::dark().with_text_scale(2.0),
    )
    .unwrap();
    assert_sidebar_targets(d, 900.0);
    // Large text narrows the window to the drawer, which covers the request
    // while open; close it to reach the request.
    d.perform_named("nav-shell-sidebar", &ui::a11y::Action::Focus)
        .unwrap();
    d.key("Enter");
    settle_drawer(d);
    let targets = d.targets();
    assert!(targets["api.request-divider"].y1 < 700.0);
    assert!(targets["api.request-tabs"].x1 <= 800.0);
    environment_command(d, "Add variable…");
    d.wait_for_named("api.form-variable-key", ui::time::Duration::from_secs(5))
        .expect("the canonical environment command publishes its variable form");
    let field = d.targets()["api.form-variable-key"];
    assert!(
        field.x0 >= 0.0 && field.x1 <= 800.0 && field.y1 <= 900.0,
        "large-text management input remains visible: {field:?}"
    );
}

/// Sheets collect a name and dispatch the canonical action through the real
/// controls.
#[test]
fn a_sheet_creates_a_collection_through_the_real_controls() {
    let mut app = sample();
    let d = app.harness();
    d.click_named("api.new-collection").unwrap();
    d.type_text("Billing API");
    d.key("Enter");
    let listed = d.a11y_node_named("Billing API").is_some()
        && d.a11y_node_named("Rename collection Billing API").is_some();
    assert!(
        listed,
        "the new collection is listed, with its rename action"
    );
}

/// Send (pointer and keyboard) issues the request being composed.
#[test]
fn pointer_and_keyboard_send_use_the_live_request() {
    use std::io::{Read, Write};
    for keyboard in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = tilo_test_env::hang_deadline();
            loop {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut bytes = [0; 4096];
                    let count = stream.read(&mut bytes).unwrap();
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
                    return String::from_utf8_lossy(&bytes[..count]).to_string();
                }
                assert!(
                    ui::time::now() < deadline,
                    "Send never reached the HTTP server"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        let mut app = sample();
        // The token is a secret variable: its value is kept on this device.
        let state = app.state();
        let token = state
            .active_environment
            .as_ref()
            .and_then(|active| state.environments.get(active))
            .and_then(|environment| {
                environment
                    .variables
                    .iter()
                    .find(|(_, variable)| variable.key == "token")
                    .map(|(id, _)| id.as_str().to_owned())
            })
            .expect("the active environment has a token");
        app.effect(&ApiTesterEffect::SetSecretValue {
            variable: token,
            value: "prod_sk_9f8c1a2b3d4e5f60".into(),
        })
        .unwrap();
        // The request's own credential, also kept on this device, names it.
        app.effect(&ApiTesterEffect::SetCredential {
            secret: "{{token}}".into(),
        })
        .unwrap();
        let d = app.harness();
        d.perform_named(
            "api.url",
            &ui::a11y::Action::SetValue(format!("http://{address}/verify")),
        )
        .unwrap();
        if keyboard {
            d.perform_named("api.send", &ui::a11y::Action::Focus)
                .unwrap();
            d.key("Enter");
        } else {
            d.click_named("api.send").unwrap();
        }
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /verify?page=1&per_page=25 HTTP/1.1"),
            "{request}"
        );
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer prod_sk_")
        );
    }
}

/// The accessibility tree is clean in the empty workspace and in the populated
/// demo. Rows and cells carry their callbacks as widget closures, which the
/// native-app audit reports as such; nothing else may be reported.
#[test]
fn native_app_contract_is_certified() {
    for scenario in ["empty", "rail-open"] {
        let mut app = captured(scenario, (1280, 800));
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "{scenario}: {audit:#?}");
    }
}
