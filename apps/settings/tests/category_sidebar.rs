//! Asserts the sidebar advertises one entry per category.
//!
//! Constructs `SettingsApp`, runs `build_layout` + `apply_layout` at a
//! wide size (so the two-pane layout with the full sidebar is built), then
//! walks the widget tree counting the sidebar rows. Each sidebar row is a
//! `SidebarRow` addressable by its category's display label via `test_id`,
//! so the count is exact (the content pane contains no `SidebarRow`s).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use settings::SettingsApp;
use settings::categories::Category;
use std::collections::HashMap;
use taffy::{NodeId, TaffyTree};
use ui::widget::Widget;

#[test]
fn sidebar_has_one_entry_per_category() {
    let mut app = SettingsApp::new();
    let theme = ui::Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

    let node = app.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::length(1200.0),
                    height: taffy::Dimension::length(760.0),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    taffy
        .compute_layout(
            root,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(1200.0),
                height: taffy::AvailableSpace::Definite(760.0),
            },
        )
        .unwrap();
    app.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);

    // Each sidebar entry is a `SidebarRow` exposing its category's full
    // display label as `test_id`. Collect those labels and assert exactly one
    // row per category, with the *complete* (untruncated) label.
    let mut labels: Vec<String> = Vec::new();
    walk(&app, &mut |w| {
        if w.name() == "SidebarRow"
            && let Some(l) = w.test_id()
        {
            labels.push(l.to_string());
        }
    });

    assert_eq!(
        labels.len(),
        Category::ALL.len(),
        "expected exactly one sidebar row per category"
    );

    for category in Category::ALL {
        let found = labels
            .iter()
            .filter(|l| l.as_str() == category.label())
            .count();
        assert_eq!(
            found,
            1,
            "expected one sidebar row carrying the full label {:?} for {:?}",
            category.label(),
            category
        );
    }
}

fn walk<F: FnMut(&dyn Widget)>(w: &dyn Widget, f: &mut F) {
    f(w);
    for child in w.children() {
        walk(child, f);
    }
}
