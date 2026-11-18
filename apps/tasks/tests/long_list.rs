//! The Tasks window over a tracker of thousands of issues: the list reads
//! the tree once and builds only the rows its window shows, and a reader still
//! hears each row's place among all of them.
//!
//! The row counts are asserted; the times are printed for reading
//! (`cargo test -p tasks --test long_list -- --nocapture`), never asserted:
//! on a shared machine they vary with the load, the counts do not.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_precision_loss,
    reason = "in a test these are the assertion, not a hazard"
)]

mod common;

use common::{chain, held};
use serde_json::{Value, json};
use tasks::App;
use ui::prelude::*;

const ISSUES: usize = 2_500;

/// Every row of the list a reader meets, named by its issue's reference
/// (`#12  …`), with the count of rows it says it is one of. The sidebar's
/// filters are a tree too, without numbers.
fn tree_items(app: &mut TestApp<App>) -> Vec<(String, Option<u64>)> {
    fn walk(value: &Value, out: &mut Vec<(String, Option<u64>)>) {
        match value {
            Value::Object(node) => {
                let name = node.get("name").and_then(Value::as_str).unwrap_or_default();
                if node.get("role").and_then(Value::as_str) == Some("TreeItem")
                    && name.starts_with('#')
                {
                    out.push((
                        name.to_owned(),
                        node.get("set_size").and_then(Value::as_u64),
                    ));
                }
                node.values().for_each(|child| walk(child, out));
            }
            Value::Array(children) => children.iter().for_each(|child| walk(child, out)),
            _ => {}
        }
    }
    let snapshot: Value =
        serde_json::from_str(&app.harness().inspection_json().expect("inspection")).expect("json");
    let mut out = Vec::new();
    walk(&snapshot, &mut out);
    out
}

/// Choose the sidebar's `name` row, as a press on it does.
fn choose(app: &mut TestApp<App>, name: &str) {
    let harness = app.harness();
    let node = harness
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("the sidebar offers {name:?}"));
    harness
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("{name} is chosen: {error}"));
    let _ = harness.poll_frame();
}

/// The window is 780 px tall: a few dozen rows show, with a margin.
fn assert_a_screenful(rows: &[(String, Option<u64>)], when: &str) {
    assert!(
        (1..=150).contains(&rows.len()),
        "{when}, the list of {ISSUES} issues holds {} rows",
        rows.len()
    );
}

#[test]
fn a_long_trackers_list_builds_a_screenful_of_rows_and_names_each_rows_place() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<App>::drive(&format!("issues-{ISSUES}")).expect("open");
    assert_eq!(held(&app).len(), ISSUES);
    let rows = tree_items(&mut app);
    assert_a_screenful(&rows, "opened");
    // A row's own controls stand inside it; the rows themselves say where
    // among all the issues they stand.
    let all = u64::try_from(ISSUES).unwrap();
    let placed: Vec<_> = rows.iter().filter_map(|(_, size)| *size).collect();
    assert!(
        !placed.is_empty() && placed.iter().all(|size| *size == all),
        "each row says it is one of all {ISSUES}: {placed:?}"
    );

    // Another filter reads the tree again and builds the list anew.
    let started = std::time::Instant::now();
    choose(&mut app, "Todos");
    let filtered = started.elapsed();
    assert_a_screenful(&tree_items(&mut app), "filtered");

    // Each new issue reads the part it is written into and lists the rows that moved.
    let mut created = Vec::new();
    for title in ["One more", "And another", "And a third"] {
        let started = std::time::Instant::now();
        create_issue(&mut app, title);
        created.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_a_screenful(&tree_items(&mut app), "with an issue added");
    }
    assert_eq!(held(&app).len(), ISSUES + 3);

    // The board builds the cards its columns show, and takes an issue as the list does.
    let started = std::time::Instant::now();
    press_view(&mut app, "Board");
    app.settle();
    let board = started.elapsed().as_secs_f64() * 1000.0;
    let cards = cards(&mut app);
    assert!(
        (1..=150).contains(&cards),
        "the board of {ISSUES} issues holds {cards} cards built"
    );
    let started = std::time::Instant::now();
    app.call_on(&chain(), "create", json!({ "title": "On the board" }))
        .expect("create");
    app.settle();
    let on_board = started.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{ISSUES} issues: the list rebuilt for a filter in {} ms; a new issue listed in {} ms; \
         the board opened in {board:.0} ms with {cards} cards; a new issue on the board in \
         {on_board:.0} ms",
        filtered.as_millis(),
        created
            .iter()
            .map(|ms| format!("{ms:.0}"))
            .collect::<Vec<_>>()
            .join(" / "),
    );
}

/// Add an issue titled `title` as a caller does. The call resolves the one resource it names (the
/// chain) and never builds the tree of every issue to find it.
#[inline(never)]
fn create_issue(app: &mut TestApp<App>, title: &str) {
    let ((), scopes) = ui::tilo_resource::profiling::observe(|| {
        app.call_on(&chain(), "create", json!({ "title": title }))
            .expect("create");
        app.settle();
    });
    let whole: Vec<_> = scopes
        .iter()
        .filter(|scope| {
            matches!(
                scope.phase,
                "framework.resources_for" | "framework.resource_for"
            )
        })
        .map(|scope| (scope.phase, scope.calls))
        .collect();
    assert!(
        whole.is_empty(),
        "adding an issue built the whole published tree: {whole:?}"
    );
}

/// The cards the board holds built: each named by its issue's reference.
fn cards(app: &mut TestApp<App>) -> usize {
    fn walk(value: &Value, count: &mut usize) {
        match value {
            Value::Object(node) => {
                let name = node.get("name").and_then(Value::as_str).unwrap_or_default();
                if name.starts_with('#')
                    && node.get("role").and_then(Value::as_str) != Some("TreeItem")
                {
                    *count = count.saturating_add(1);
                }
                node.values().for_each(|child| walk(child, count));
            }
            Value::Array(children) => children.iter().for_each(|child| walk(child, count)),
            _ => {}
        }
    }
    let snapshot: Value =
        serde_json::from_str(&app.harness().inspection_json().expect("inspection")).expect("json");
    let mut count = 0;
    walk(&snapshot, &mut count);
    count
}

/// Press the window bar's view `name`, as a press on it does.
fn press_view(app: &mut TestApp<App>, name: &str) {
    let harness = app.harness();
    let bar = ui::window_toolbar::declared().expect("the window declares its views");
    let views = bar.views.expect("the window declares its views");
    let view = views
        .choices
        .iter()
        .find(|view| view.label == name)
        .expect("the bar offers the view");
    harness.press_window_part(ui::window_toolbar::ToolbarEvent::Action {
        id: view.id.clone(),
    });
    let _ = harness.poll_frame();
}

/// History lists the issues' changes, kept in the tracker's parts, with the tracker's own: a page
/// of the newest, and the rest a page at a time.
#[test]
fn history_lists_the_issues_changes_a_page_at_a_time() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<App>::for_person("ada").expect("open");
    for at in 0..=ui::history_mode::HISTORY_PAGE {
        app.call_on(
            &chain(),
            "create",
            json!({ "title": format!("Issue {at}") }),
        )
        .expect("create");
    }
    app.settle();
    let harness = app.harness();
    harness.open_history().expect("History opens");
    assert!(harness.targets().contains_key("activity_timeline"));
    assert!(harness.targets().contains_key("history-earlier"));
    harness.click_named("history-earlier").unwrap();
    assert!(!harness.targets().contains_key("history-earlier"));
}
