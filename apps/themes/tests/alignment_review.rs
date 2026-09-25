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
use themes::customizer::{
    chip_grid_height, chip_rows_for_width, intrinsic_height_for_width,
    intrinsic_height_for_width_scaled,
};
use themes::model::default_catalog;
use themes::widgets::{ROW_H, ThemeRow};
use ui::text_render::TextRenderer;
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};

/// The production layout cycle for any widget under a large-text
/// preference: the theme carries `text_scale` and the renderer is told the
/// same scale, exactly as the live host and the shared launcher do. The
/// outer box is a column so a measured, auto-height child keeps its own
/// height instead of being stretched to the box.
fn lay_out_widget_scaled(widget: &mut dyn Widget, (w, h): (f32, f32), text_scale: f32) {
    let theme = ui::Theme::dark().with_text_scale(text_scale);
    let mut state = ui::State::new();
    widget.sync(&mut state.doc, &state.root);
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = widget.build_layout(&mut taffy, &theme, &mut node_map);
    let outer = taffy
        .new_with_children(
            taffy::Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
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
    tr.set_text_scale(text_scale);
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
    widget.apply_layout(&taffy, node, Point::ORIGIN);
}

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
    for (name, button) in [
        ("Duplicate", duplicate),
        ("Reset", reset),
        ("Delete", delete),
    ] {
        close(
            button.height(),
            font.height(),
            &format!("{name} button height vs the font dropdown"),
        );
        close(
            button.y0,
            duplicate.y0,
            &format!("{name} shares the action row"),
        );
    }
    // The chooser and the actions share the panel's left inset.
    close(
        duplicate.x0,
        font.x0,
        "actions start on the dropdown's left edge",
    );
}

// ---- round two (2026-09-17) ----

/// The overlay gallery's toggle is the shared icon-only `Button`, laid out
/// in the top bar's leading slot: a `bar_height` square on the content
/// gutter, centred in the bar, on the same centre line and control height as
/// the bar's actions (round one's U5 replaced a hand-painted 34 px rect).
#[test]
fn gallery_toggle_is_a_shared_button_on_the_bar_control_height() {
    let mut app = narrow_customize_app();
    lay_out(&mut app, (480.0, 900.0));
    let metrics = ui::tokens::Metrics::default();
    let toggle = by_test_id(&app, "themes.gallery_toggle");
    close(
        toggle.width(),
        f64::from(metrics.bar_height),
        "toggle width",
    );
    close(
        toggle.height(),
        f64::from(metrics.bar_height),
        "toggle height",
    );
    close(
        toggle.x0,
        f64::from(NARROW_CONTENT_GUTTER),
        "toggle starts on the content gutter",
    );
    close(
        toggle.center().y,
        f64::from(TOPBAR_H) / 2.0,
        "toggle centred in the 1× bar",
    );
    for id in ["themes.new", "themes.export", "themes.apply"] {
        let action = by_test_id(&app, id);
        close(
            action.center().y,
            toggle.center().y,
            &format!("{id} shares the toggle's centre line"),
        );
        close(
            action.height(),
            toggle.height(),
            &format!("{id} shares the control height"),
        );
    }
    // The toggle is a real child of the tree (reachable by assistive
    // technology), not a painted rect, and it speaks the action it will
    // perform: showing a hidden gallery here…
    let spoken = |app: &ThemesApp| {
        app.children()
            .iter()
            .find(|c| c.test_id() == Some("themes.gallery_toggle"))
            .and_then(|c| c.accessibility().and_then(|n| n.name))
            .unwrap_or_else(|| panic!("the toggle is a child widget of the app"))
    };
    assert_eq!(spoken(&app), "Show themes");
    // …and hiding a shown one. The "menu" shot opens the gallery flap, so
    // the toggle in that tree is the close control.
    let mut open = ThemesApp::new_demo();
    open.seed_shot(480.0, "menu");
    lay_out(&mut open, (480.0, 900.0));
    assert_eq!(spoken(&open), "Hide themes");
    let close_toggle = by_test_id(&open, "themes.gallery_toggle");
    close(
        close_toggle.x0,
        toggle.x0,
        "the close control keeps the toggle's slot",
    );
    close(
        close_toggle.width(),
        toggle.width(),
        "the close control keeps the toggle's size",
    );
}

/// Under large text on a phone-width window the bar's actions no longer fit
/// beside the toggle. They wrap onto their own line(s) inside the window
/// instead of being painted over the toggle, and the content starts below
/// the taller bar.
#[test]
fn top_bar_wraps_its_actions_under_large_text_instead_of_covering_the_toggle() {
    let mut app = narrow_customize_app();
    lay_out_widget_scaled(&mut app, (480.0, 900.0), 2.0);
    let toggle = by_test_id(&app, "themes.gallery_toggle");
    let mut bar_bottom = toggle.y1;
    for id in ["themes.new", "themes.export", "themes.apply"] {
        let action = by_test_id(&app, id);
        let overlap = toggle.intersect(action);
        assert!(
            overlap.width() <= 0.0 || overlap.height() <= 0.0,
            "{id} {action:?} must not cover the toggle {toggle:?}"
        );
        assert!(
            action.x0 >= 0.0 && action.x1 <= 480.0,
            "{id} {action:?} stays inside the window"
        );
        bar_bottom = bar_bottom.max(action.y1);
    }
    let tabs = by_widget_name(&app, "ToggleGroup")[0];
    assert!(
        tabs.y0 >= bar_bottom,
        "the view selector {tabs:?} starts below the wrapped bar (bottom {bar_bottom})"
    );
}

/// A gallery row is exactly its 1× height at 1× and grows from its measured
/// name and tag lines under large text, so the tag no longer stacks over the
/// name (round one's U6).
#[test]
fn gallery_rows_keep_their_1x_height_and_grow_with_large_text() {
    let spec = default_catalog()
        .into_iter()
        .next()
        .expect("a preset theme");
    let mut row = ThemeRow::from_spec(&spec, false);
    lay_out_widget_scaled(&mut row, (272.0, 400.0), 1.0);
    close(row.get_rect().height(), f64::from(ROW_H), "1× row height");
    close(row.get_rect().width(), 272.0, "row fills the gallery width");

    let mut large = ThemeRow::from_spec(&spec, false);
    lay_out_widget_scaled(&mut large, (272.0, 400.0), 2.0);
    let tall = large.get_rect().height();
    assert!(
        tall > f64::from(ROW_H) + 20.0,
        "200 % row ({tall}) seats two doubled text lines, not the 1× {ROW_H}"
    );
    // The inline-editor rect follows the drawn name line.
    let name = large.name_rect();
    assert!(
        name.y0 >= large.get_rect().y0 && name.y1 <= large.get_rect().y1,
        "name rect {name:?} stays inside the row {:?}",
        large.get_rect()
    );
}

/// The specimen's text-driven sections and the customizer's hand-laid
/// header, caption and control labels follow the text scale, so the narrow
/// stack reflows instead of overprinting itself (round one's U6).
#[test]
fn specimen_and_customizer_reflow_with_large_text() {
    assert!(
        intrinsic_height_for_width_scaled(480.0, 2.0) > intrinsic_height_for_width(480.0) + 100.0,
        "the customizer's intrinsic height grows with the type"
    );
    close(
        f64::from(intrinsic_height_for_width_scaled(480.0, 1.0)),
        f64::from(intrinsic_height_for_width(480.0)),
        "1× is exactly the unscaled height",
    );

    let mut one = ThemesApp::new_demo();
    one.seed_shot(480.0, "preview");
    lay_out_widget_scaled(&mut one, (480.0, 900.0), 1.0);
    let specimen_1x = by_widget_name(&one, "PreviewPane")[0];

    let mut two = ThemesApp::new_demo();
    two.seed_shot(480.0, "preview");
    lay_out_widget_scaled(&mut two, (480.0, 900.0), 2.0);
    let specimen_2x = by_widget_name(&two, "PreviewPane")[0];
    assert!(
        specimen_2x.height() > specimen_1x.height() + 200.0,
        "the full-preview specimen grows with the type ({} → {})",
        specimen_1x.height(),
        specimen_2x.height()
    );
}

/// The gallery is one rhythm (HIG §7b): every row sits exactly one `s2` —
/// the gallery's related-rows step — below the row above it, inset `s4`
/// from the pane on both sides, and no two rows ever touch (the 1× polish
/// bar). Measured on the real docked layout, not on the constants.
#[test]
fn gallery_rows_stack_on_the_s2_step_and_never_touch() {
    let sp = ui::tokens::Spacing::BASE;
    let mut app = ThemesApp::new_demo();
    app.seed_shot(1280.0, "");
    lay_out(&mut app, (1280.0, 1500.0));
    let mut rows = by_widget_name(&app, "ThemeRow");
    rows.retain(|r| r.width() > 0.0 && r.height() > 0.0);
    rows.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    assert!(
        rows.len() >= 3,
        "the demo gallery lists its presets and the custom theme, found {}",
        rows.len()
    );
    for pair in rows.windows(2) {
        let gap = pair[1].y0 - pair[0].y1;
        assert!(
            gap >= f64::from(sp.s1),
            "gallery rows touch: {:?} → {:?} (gap {gap})",
            pair[0],
            pair[1]
        );
        close(gap, f64::from(sp.s2), "row to row");
        close(pair[0].x0, pair[1].x0, "rows share a left edge");
        close(pair[0].x1, pair[1].x1, "rows share a right edge");
    }
    let gallery = by_widget_name(&app, "Gallery")[0];
    close(
        rows[0].x0 - gallery.x0,
        f64::from(sp.s4),
        "row inset from the gallery edge",
    );
    close(
        gallery.x1 - rows[0].x1,
        f64::from(sp.s4),
        "row inset from the gallery edge",
    );
}
