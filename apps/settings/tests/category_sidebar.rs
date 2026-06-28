//! Asserts the sidebar advertises the expected categories.
//!
//! Spec from the skeleton brief: "at least one test that constructs
//! `SettingsApp`, runs `build_layout` + `apply_layout`, and asserts the
//! category sidebar has the expected count of entries". We do exactly
//! that here — the `Category::ALL` slice is the canonical source of
//! truth, so the assertion uses its length.

use std::collections::HashMap;
use settings::categories::Category;
use settings::SettingsApp;
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
                    width: taffy::Dimension::length(1000.0),
                    height: taffy::Dimension::length(700.0),
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
                width: taffy::AvailableSpace::Definite(1000.0),
                height: taffy::AvailableSpace::Definite(700.0),
            },
        )
        .unwrap();
    app.apply_layout(&taffy, node, vello::kurbo::Point::ORIGIN);

    // Walk the widget tree and collect every node whose name is
    // "Button" — there's the per-category sidebar Button list. Since
    // the only Buttons in the skeleton today are the sidebar entries,
    // the count must equal `Category::ALL.len()`.
    let mut buttons = 0usize;
    walk(&app, &mut |w| {
        if w.name() == "Button" {
            buttons += 1;
        }
    });

    assert_eq!(
        buttons,
        Category::ALL.len(),
        "expected one sidebar Button per category"
    );
}

fn walk<F: FnMut(&dyn Widget)>(w: &dyn Widget, f: &mut F) {
    f(w);
    for child in w.children() {
        walk(child, f);
    }
}
