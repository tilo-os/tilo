//! Geometry regressions from the 2026-09-16 UI alignment review (unit A3).
//!
//! Themes lays its panes out by hand in taffy; these tests run the real
//! `ThemesApp` through `sync → build_layout → compute_layout → apply_layout`
//! and check the resulting rectangles against the app's named gutters and the
//! shared control heights.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use themes::ThemesApp;
use themes::app::{NARROW_CONTENT_GUTTER, TOPBAR_H};
use themes::customizer::{chip_grid_height, chip_rows_for_width, intrinsic_height_for_width};
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};

fn lay_out(app: &mut ThemesApp, (w, h): (f32, f32)) {
    let theme = ui::Theme::dark();
    let mut state = ui::State::new();
    app.sync(&mut state.doc, &state.root);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = app.build_layout(&mut taffy, &theme, &mut node_map);
    let outer = taffy
        .new_with_children(
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::length(w),
                    height: taffy::Dimension::length(h),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    let measure_map = node_map.clone();
    let mut tr = TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            outer,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(w),
                height: taffy::AvailableSpace::Definite(h),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = measure_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .unwrap();
    app.apply_layout(&taffy, node, Point::ORIGIN);
}

fn walk(w: &dyn Widget, f: &mut dyn FnMut(&dyn Widget)) {
    f(w);
    for child in w.children() {
        walk(child, f);
    }
}

fn by_test_id(root: &dyn Widget, id: &str) -> Rect {
    let mut out = None;
    walk(root, &mut |w| {
        if out.is_none() && w.test_id() == Some(id) {
            out = Some(w.get_rect());
        }
    });
    out.unwrap_or_else(|| panic!("no widget with test id {id:?}"))
}

fn by_widget_name(root: &dyn Widget, name: &str) -> Vec<Rect> {
    let mut out = Vec::new();
    walk(root, &mut |w| {
        if w.name() == name {
            out.push(w.get_rect());
        }
    });
    out
}

fn close(a: f64, b: f64, what: &str) {
    assert!(
        (a - b).abs() < 0.5,
        "{what}: expected {b}, got {a} (difference {})",
        a - b
    );
}

fn narrow_customize_app() -> ThemesApp {
    let mut app = ThemesApp::new_demo();
    app.seed_shot(480.0, "customize");
    app
}

#[test]
fn narrow_view_selector_keeps_the_content_gutter_on_three_sides() {
    let mut app = narrow_customize_app();
    lay_out(&mut app, (480.0, 900.0));
    let tabs = by_widget_name(&app, "ToggleGroup")[0];
    let gutter = f64::from(NARROW_CONTENT_GUTTER);
    // The selector is content-sized and left-aligned to the content gutter
    // (the accepted narrow view-selector alignment); above it the same gutter
    // separates it from the top bar's hairline.
    close(tabs.x0, gutter, "selector left gutter");
    assert!(
        tabs.x1 <= 480.0 - gutter,
        "selector {tabs:?} stays inside the right gutter"
    );
    close(
        tabs.y0,
        f64::from(TOPBAR_H) + gutter,
        "selector no longer sits flush on the top bar hairline",
    );
}

#[test]
fn customizer_chip_grid_wraps_to_the_width_it_is_given() {
    // Docked column: 344 − 2·16 = 312 px of content → two chip rows.
    assert_eq!(chip_rows_for_width(312.0), 2);
    // Narrow window: 480 − 2·16 = 448 px → every chip fits on one row.
    assert_eq!(chip_rows_for_width(448.0), 1);
    // One fewer row is exactly one chip (28) plus one gap (8) shorter, so the
    // caption follows the chips instead of leaving a dead band beneath them.
    close(
        f64::from(chip_grid_height(312.0) - chip_grid_height(448.0)),
        28.0 + 8.0,
        "grid height difference between two rows and one",
    );
    assert!(
        intrinsic_height_for_width(480.0) < intrinsic_height_for_width(344.0),
        "the stacked panel's intrinsic height follows the chip rows"
    );
}

#[test]
fn customizer_actions_match_the_dropdown_control_height() {
    let mut app = narrow_customize_app();
    lay_out(&mut app, (480.0, 900.0));
    let duplicate = by_test_id(&app, "themes.duplicate");
    let reset = by_test_id(&app, "themes.reset");
    let delete = by_test_id(&app, "themes.delete");
    let font = by_test_id(&app, "font_dropdown");
    for (name, button) in [("Duplicate", duplicate), ("Reset", reset), ("Delete", delete)] {
        close(
            button.height(),
            font.height(),
            &format!("{name} button height vs the font dropdown"),
        );
        close(button.y0, duplicate.y0, &format!("{name} shares the action row"));
    }
    // The chooser and the actions share the panel's left inset.
    close(duplicate.x0, font.x0, "actions start on the dropdown's left edge");
}
