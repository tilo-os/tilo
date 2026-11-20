//! What the list shows where the person is: the issue a press in its field just made,
//! and its top when issues land above it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use serde_json::json;
use tasks::App;
use ui::prelude::*;

mod common;
use common::{chain, held};

fn open() -> TestApp<App> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<App>::for_person("ada").expect("open")
}

/// The list's rows on screen, by the name each answers, top first.
fn rows(app: &mut TestApp<App>) -> Vec<(String, Rect)> {
    let mut rows: Vec<(String, Rect)> = app
        .harness()
        .targets()
        .into_iter()
        .filter(|(name, _)| name.starts_with("key:"))
        .collect();
    rows.sort_by(|a, b| a.1.y0.total_cmp(&b.1.y0));
    rows
}

/// The list's own box, the band its rows are drawn in.
fn list_box(app: &mut TestApp<App>) -> Rect {
    app.harness()
        .targets()
        .get("tasks-list")
        .copied()
        .expect("the list stands")
}

/// Whether the row named `key` is drawn within the list's box.
fn on_screen(app: &mut TestApp<App>, key: &str) -> bool {
    let band = list_box(app);
    app.harness()
        .targets()
        .get(&format!("key:{key}"))
        .is_some_and(|row| row.y0 >= band.y0 - 0.5 && row.y1 <= band.y1 + 0.5)
}

/// The first row the list's box shows, by its name.
fn top_row(app: &mut TestApp<App>) -> Option<String> {
    let band = list_box(app);
    rows(app)
        .into_iter()
        .filter(|(_, row)| row.y0 >= band.y0 - 0.5 && row.y0 < band.y1)
        .min_by(|a, b| a.1.y0.total_cmp(&b.1.y0))
        .map(|(name, _)| name)
}

/// A tracker of `count` issues, made as the workspace arriving fills one.
fn fill(app: &mut TestApp<App>, count: usize) {
    let issues: Vec<serde_json::Value> = (0..count)
        .map(|i| json!({ "title": format!("issue {i}") }))
        .collect();
    app.call_on(&chain(), "create-many", json!({ "issues": issues }))
        .expect("create-many");
    app.settle();
}

#[test]
fn an_issue_typed_in_the_field_is_shown_where_it_lands() {
    let mut app = open();
    fill(&mut app, 100);
    assert_eq!(
        top_row(&mut app).as_deref(),
        Some("key:1"),
        "the list opens at its top"
    );
    assert!(
        !on_screen(&mut app, "101"),
        "the new issue is not there yet"
    );

    app.harness()
        .click_named("tasks-list-new-title")
        .expect("focus the title");
    app.harness().type_text("Ship the typed issue");
    app.harness().key("Enter");
    app.settle();
    assert_eq!(held(&app).len(), 101, "the press made the issue");
    assert!(
        on_screen(&mut app, "101"),
        "the issue the press made is shown: {:?}",
        rows(&mut app).last()
    );
    let chosen = app.harness().a11y_nodes().into_iter().any(|(_, node)| {
        node.is_selected() == Some(true)
            && node
                .label()
                .is_some_and(|label| label.contains("Ship the typed issue"))
    });
    assert!(chosen, "the row the press made stands chosen");
    assert!(
        app.harness().refusal_said().is_none(),
        "nothing is said over the window"
    );
}

#[test]
fn a_new_issue_typed_under_a_filter_still_shows() {
    let mut app = open();
    fill(&mut app, 12);
    // The sidebar narrows the list to the person's own issues, which the new one is not:
    // a press there shows every issue, where it made this one.
    let harness = app.harness();
    let node = harness
        .a11y_node_named("Mine")
        .expect("the sidebar offers the mine filter");
    harness
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .expect("the filter is chosen");
    let _ = harness.poll_frame();
    app.settle();
    assert!(
        rows(&mut app).is_empty(),
        "nobody's issues are the person's yet"
    );

    app.harness()
        .click_named("tasks-list-new-title")
        .expect("focus the title");
    app.harness().type_text("An issue of my own");
    app.harness().key("Enter");
    app.settle();
    assert!(
        on_screen(&mut app, "13"),
        "the issue made under the filter is shown: {:?}",
        rows(&mut app).last()
    );
}

#[test]
fn the_list_keeps_its_top_when_an_issue_lands_above_it() {
    let mut app = open();
    fill(&mut app, 100);
    let ids = held(&app).ids();
    let (first, last) = (ids[0].clone(), ids[99].clone());
    // An issue that reaches the list from above where it stands now (an older part
    // arriving after the newest) must not scroll the window down with it.
    app.call_on(
        &tasks::model::issue_resource(&last),
        "place",
        json!({ "status": "todo", "before": first.as_str() }),
    )
    .expect("placed first");
    app.settle();
    assert_eq!(
        top_row(&mut app).as_deref(),
        Some("key:100"),
        "the top now shows the issue that landed above it"
    );
    assert!(on_screen(&mut app, "1"), "the next row follows it");
}
