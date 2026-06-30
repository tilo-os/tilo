//! Asserts the sidebar advertises one entry per category.
//!
//! Constructs `SettingsApp`, runs `build_layout` + `apply_layout` at a
//! wide size (so the two-pane layout with the full sidebar is built), then
//! walks the widget tree counting the sidebar buttons. Sidebar buttons are
//! addressable by their `settings.sidebar.<slug>` test id, so the count is
//! exact even though the content pane also contains buttons.

use settings::categories::Category;
use settings::SettingsApp;
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

    // `ui` Buttons expose their *label* as `test_id`. Every sidebar entry
    // is a Button whose label ends with its category's display label
    // (the label is "<marker> <glyph>  <Category::label()>"), so we count
    // a sidebar button for each category whose label appears as a Button
    // label exactly once.
    let mut labels: Vec<String> = Vec::new();
    walk(&app, &mut |w| {
        if w.name() == "Button" {
            if let Some(l) = w.test_id() {
                labels.push(l.to_string());
            }
        }
    });

    let mut matched = 0usize;
    for category in Category::ALL {
        let found = labels.iter().filter(|l| l.ends_with(category.label())).count();
        assert!(
            found >= 1,
            "expected a sidebar button for {:?} (label {:?})",
            category,
            category.label()
        );
        matched += 1;
    }

    assert_eq!(
        matched,
        Category::ALL.len(),
        "expected one sidebar button per category"
    );
}

fn walk<F: FnMut(&dyn Widget)>(w: &dyn Widget, f: &mut F) {
    f(w);
    for child in w.children() {
        walk(child, f);
    }
}
