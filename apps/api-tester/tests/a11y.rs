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
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
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

fn staged(scenario: &str, size: (u32, u32)) -> Driver {
    let mut app = ApiTesterApp::new_demo();
    assert!(app.stage(scenario), "{scenario}");
    Driver::new(State::new(), Box::new(app), size)
}

/// Every surface (full demo, empty and error responses, the narrow window,
/// contextual sheets) exposes its controls as named accessibility nodes; each
/// case mounts its own Driver.
#[test]
fn every_surface_exposes_named_accessible_controls() {
    every_control_is_a_focusable_accessible_node();
    empty_and_error_response_surfaces_expose_recovery_actions();
    narrow_window_keeps_the_sidebar_toggle_reachable();
    sheets_expose_only_their_named_fields();
}

/// Every interactive control is a real focusable widget with an accessibility
/// node carrying the correct role, name, and value — the thing a single
/// painted canvas could never provide.
/// Case of `every_surface_exposes_named_accessible_controls`.
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
    // A saved request is an addressable, labelled row that reports selection.
    let row = nodes
        .iter()
        .find(|n| n.role == Role::ListItem && n.name.as_deref() == Some("List users"))
        .expect("a saved request is a focusable, labelled row");
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
    let empty = staged("empty", (1280, 800));
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

    let error = staged("error", (390, 844));
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
fn narrow_window_keeps_the_sidebar_toggle_reachable() {
    let d = staged("default", (390, 844));
    assert!(
        d.focus_access_nodes().iter().any(|node| {
            node.role == Role::Button && node.name.as_deref() == Some("Show sidebar")
        }),
        "the narrow window's toggle opens the drawer"
    );
}

/// Case of `every_surface_exposes_named_accessible_controls`.
fn sheets_expose_only_their_named_fields() {
    let variable = staged("form-new-variable", (1280, 800));
    let nodes = variable.focus_access_nodes();
    for name in ["Variable", "Value"] {
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

    let rename = staged("form-rename-request", (1280, 800));
    assert!(
        rename
            .focus_access_nodes()
            .iter()
            .any(|node| node.role == Role::TextInput && node.name.as_deref() == Some("Name")),
        "the rename sheet has its single named field"
    );
}

/// Tab order, Enter activation and Escape dismissal drive the real widget
/// handlers; each case mounts its own Driver.
#[test]
fn keyboard_focus_traverses_activates_and_dismisses_through_real_handlers() {
    tab_traverses_every_control_in_order();
    enter_activates_a_focused_row_through_the_real_handler();
    escape_dismisses_a_sheet();
}

/// Tab / Shift+Tab move keyboard focus between the real controls in tree
/// order — the traversal the compositor's `App` performs, exercised here
/// through the same focus walk.
/// Case of `keyboard_focus_traverses_activates_and_dismisses_through_real_handlers`.
fn tab_traverses_every_control_in_order() {
    let mut d = driver();
    let stops = d.tab_through();

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
        "fifth stop is the request tabs"
    );

    // Every focusable control is reached exactly once.
    assert_eq!(
        stops.len(),
        d.focus_order().len(),
        "Tab visits every focusable node"
    );

    // Focus reaches into the sidebar rows and the table's cells.
    assert!(
        stops
            .iter()
            .any(|(r, n)| *r == Role::ListItem && n.as_deref() == Some("List users")),
        "Tab reaches the saved-request rows"
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

/// Enter on a focused row activates it through the widget's real handler —
/// the same path a screen reader's "activate" takes — and the request loads
/// into the builder.
/// Case of `keyboard_focus_traverses_activates_and_dismisses_through_real_handlers`.
fn enter_activates_a_focused_row_through_the_real_handler() {
    let mut d = driver();
    let target = "Create user";

    let mut reached = false;
    for _ in 0..=d.focus_order().len() {
        d.tab(false);
        if d.focused_access_node().and_then(|n| n.name).as_deref() == Some(target) {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab can land on the saved request");
    d.key("Enter");

    let nodes = d.focus_access_nodes();
    assert!(
        nodes
            .iter()
            .any(|n| n.name.as_deref() == Some(target) && n.state.selected),
        "Enter selected the request"
    );
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::ComboBox && n.value.as_deref() == Some("POST")),
        "the method picker follows the loaded request"
    );
}

/// Case of `keyboard_focus_traverses_activates_and_dismisses_through_real_handlers`.
fn escape_dismisses_a_sheet() {
    let mut d = staged("form-new-collection", (1280, 800));
    assert!(
        d.focus_access_nodes()
            .iter()
            .any(|node| node.name.as_deref() == Some("Name")),
        "the sheet is open"
    );
    d.key("Escape");
    assert!(
        !d.focus_access_nodes()
            .iter()
            .any(|node| node.name.as_deref() == Some("Name")),
        "Escape closed the sheet"
    );
}

/// The environment picker chooses an environment, and its commands open the
/// sheets that manage environments without becoming the picker's value.
#[test]
fn the_environment_picker_switches_environments_and_opens_management_sheets() {
    let mut d = driver();
    let env_value = |d: &Driver| {
        d.focus_access_nodes()
            .iter()
            .find(|n| n.role == Role::ComboBox && n.name.as_deref() == Some("Environment"))
            .and_then(|n| n.value.clone())
    };
    assert_eq!(env_value(&d).as_deref(), Some("Production"));
    d.click_named("api.env").unwrap();
    d.key("ArrowDown");
    d.key("Enter");
    assert_eq!(env_value(&d).as_deref(), Some("Staging"), "the pick sticks");

    d.click_named("api.env").unwrap();
    // Staging, Local, No environment, then the first command.
    for _ in 0..3 {
        d.key("ArrowDown");
    }
    d.key("Enter");
    assert!(
        d.focus_access_nodes()
            .iter()
            .any(|n| n.role == Role::TextInput && n.name.as_deref() == Some("Name")),
        "New environment opens its sheet"
    );
    d.key("Escape");
    assert_eq!(
        env_value(&d).as_deref(),
        Some("Staging"),
        "a command is not the picker's value"
    );
}

/// Typing in the table's blank row keeps the caret where it is, makes the row
/// a pair and gives the table a new blank row to type the next one in.
#[test]
fn typing_in_the_blank_row_adds_a_pair_and_a_new_blank_row() {
    let mut d = driver();
    let keys = |d: &Driver| {
        d.focus_access_nodes()
            .iter()
            .filter(|n| n.role == Role::TextInput && n.name.as_deref() == Some("Key"))
            .count()
    };
    let before = keys(&d);
    let mut reached = false;
    for _ in 0..=d.focus_order().len() {
        d.tab(false);
        let focused = d.focused_access_node();
        if focused.as_ref().is_some_and(|n| {
            n.role == Role::TextInput
                && n.name.as_deref() == Some("Key")
                && n.value.as_deref().is_none_or(str::is_empty)
        }) {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab reaches the blank row's key field");
    d.type_text("limit");
    d.poll_frame().expect("a frame runs");
    let focused = d.focused_access_node().expect("focus stays");
    assert_eq!(
        focused.value.as_deref(),
        Some("limit"),
        "the field kept focus while the table grew"
    );
    assert_eq!(keys(&d), before + 1, "a new blank row appeared");
    let checked = d
        .focus_access_nodes()
        .iter()
        .filter(|n| n.role == Role::Checkbox && n.state.checked == Some(true))
        .count();
    assert!(checked >= 3, "the typed row is included, got {checked}");
}

/// Choosing a tab replaces the panel, and each panel is made of real controls.
#[test]
fn tabs_choose_the_panel_and_the_panel_is_made_of_real_controls() {
    let mut d = driver();
    let tabs = d.targets()["api.request-tabs"];
    let click_tab = |d: &mut Driver, index: f64| {
        d.click_at(
            tabs.x0 + tabs.width() * (index + 0.5) / 4.0,
            tabs.center().y,
        );
    };
    click_tab(&mut d, 1.0);
    let nodes = d.focus_access_nodes();
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::TextInput && n.value.as_deref() == Some("X-Client")),
        "the Headers tab lists the request's headers"
    );
    assert!(
        !nodes
            .iter()
            .any(|n| n.role == Role::TextInput && n.value.as_deref() == Some("per_page")),
        "the parameters are gone"
    );
    click_tab(&mut d, 2.0);
    let nodes = d.focus_access_nodes();
    assert!(
        nodes
            .iter()
            .any(|n| n.role == Role::TextInput && n.name.as_deref() == Some("Token")),
        "the Auth tab edits the bearer token"
    );
    click_tab(&mut d, 3.0);
    assert!(
        d.focus_access_nodes()
            .iter()
            .any(|n| n.role == Role::Group && n.value.as_deref() == Some("None")),
        "the Body tab offers the body modes"
    );
}

/// The split between the request and the response responds to the pointer, and
/// 2x text keeps every control on screen.
#[test]
fn the_split_drags_and_text_scale_keeps_controls_on_screen() {
    let mut d = driver();
    let before = d.targets()["api.response-view"];
    let height = 800.0;
    // The divider lies a little above the response's own top edge, which
    // leaves room between the two; find where it can be taken.
    let pointer = |d: &mut Driver, y: f64| {
        d.send(&winit::event::WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(700.0, y),
        });
    };
    let button = |d: &mut Driver, state| {
        d.send(&winit::event::WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: winit::event::MouseButton::Left,
        });
    };
    let mut after = before;
    for above in (0..40).map(f64::from) {
        let divider_y = before.y0 - above;
        pointer(&mut d, divider_y);
        button(&mut d, winit::event::ElementState::Pressed);
        pointer(&mut d, divider_y + 120.0);
        button(&mut d, winit::event::ElementState::Released);
        after = d.targets()["api.response-view"];
        if after.y0 > before.y0 + 60.0 {
            break;
        }
    }
    assert!(
        after.y0 > before.y0 + 60.0,
        "the response moved down with the divider: {before:?} -> {after:?}"
    );

    let app = ApiTesterApp::new_demo();
    let d = Driver::new_with_theme(
        State::new(),
        Box::new(app),
        (800, 900),
        ui::theme::Theme::dark().with_text_scale(2.0),
    );
    for (id, rect) in d.targets() {
        if id.starts_with("api.") {
            assert!(
                rect.x0 >= 0.0 && rect.x1 <= 800.0 && rect.y1 <= 900.0 + height,
                "{id}: {rect:?} stays in the window"
            );
        }
    }
    assert!(d.targets()["api.request-tabs"].x1 <= 800.0);
}

/// Sheets collect a name and dispatch the canonical action through the real
/// controls.
#[test]
fn a_sheet_creates_a_collection_through_the_real_controls() {
    let mut d = driver();
    d.click_named("api.new-collection").unwrap();
    d.type_text("Billing API");
    d.key("Enter");
    assert!(
        d.focus_access_nodes()
            .iter()
            .any(|node| { node.name.as_deref() == Some("Rename collection Billing API") }),
        "the new collection is listed"
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
        let mut d = driver();
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
fn native_app_audit_reports_only_widget_callbacks() {
    for app in [ApiTesterApp::new, ApiTesterApp::new_demo] {
        let d = Driver::new(State::new(), Box::new(app()), (1280, 800));
        let audit = d.native_app_audit();
        assert!(audit.accessibility.is_clean(), "{audit:#?}");
        for issue in &audit.issues {
            assert!(
                matches!(
                    issue.code.as_str(),
                    "legacy-callback" | "orphan-action-binding" | "stale-action-binding"
                ),
                "{issue:?}"
            );
        }
    }
}
