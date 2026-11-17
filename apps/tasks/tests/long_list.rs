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

    let started = std::time::Instant::now();
    app.call_on(&chain(), "create", json!({ "title": "One more" }))
        .expect("create");
    app.settle();
    let created = started.elapsed();
    assert_a_screenful(&tree_items(&mut app), "with an issue added");
    println!(
        "{ISSUES} issues: the list rebuilt for a filter in {} ms, a new issue listed in {} ms",
        filtered.as_millis(),
        created.as_millis(),
    );
}
