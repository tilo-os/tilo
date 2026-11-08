//! One placement for every way a menu opens: the control, row or point it
//! stands against decides where its panel lands, whether it fits below, and
//! which pixels of the display its edges fall on.

use super::*;

const WINDOW: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

fn menu() -> ContextMenu {
    ContextMenu::with_items(vec![
        MenuItem::action("Rename", || {}),
        MenuItem::action("Duplicate", || {}),
        MenuItem::action("Delete", || {}),
    ])
}

/// The row "Find the last service date" in the reported capture: a wide row
/// with room below it.
fn row(y0: f64) -> Rect {
    Rect::new(25.0, y0, 505.0, y0 + 39.0)
}

#[test]
fn a_menu_opened_from_the_keyboard_stands_under_its_row() {
    let _settled = crate::motion::testing::settled();
    let mut m = menu();
    let row = row(100.0);
    m.open_anchored(row, WINDOW);
    let rect = m.get_rect();
    assert!(
        rect.y0 >= row.y1 && rect.y0 - row.y1 <= ANCHOR_GAP + 0.5,
        "directly under the row: {rect:?} vs {row:?}"
    );
    assert!(
        (rect.x0 - row.x0).abs() < 1.0,
        "hugging its leading edge: {rect:?} vs {row:?}"
    );
}

#[test]
fn a_menu_near_the_foot_of_the_window_flips_above_its_row() {
    let _settled = crate::motion::testing::settled();
    let mut m = menu();
    let row = row(540.0);
    m.open_anchored(row, WINDOW);
    let rect = m.get_rect();
    assert!(
        rect.y1 <= row.y0 && row.y0 - rect.y1 <= ANCHOR_GAP + 0.5,
        "above the row where below has no room: {rect:?} vs {row:?}"
    );
}

#[test]
fn a_menu_opened_at_the_pointer_stands_at_the_pointer() {
    let _settled = crate::motion::testing::settled();
    let mut m = menu();
    m.open_at_clamped([300.0, 200.0], WINDOW);
    let rect = m.get_rect();
    assert!((rect.x0 - 300.0).abs() < 1.0, "{rect:?}");
    assert!(
        (rect.y0 - 200.0).abs() < 1.0,
        "the pointer is the menu's corner, with no gap: {rect:?}"
    );
    assert_eq!(m.position(), Some([300.0, 200.0]));
}

#[test]
fn a_menu_opened_past_the_windows_corner_stays_inside_it() {
    let _settled = crate::motion::testing::settled();
    let mut m = menu();
    m.open_at_clamped([790.0, 590.0], WINDOW);
    let rect = m.get_rect();
    assert!(
        rect.x0 >= WINDOW.x0 && rect.x1 <= WINDOW.x1 - PADDING + 0.5,
        "{rect:?}"
    );
    assert!(
        rect.y0 >= WINDOW.y0 && rect.y1 <= WINDOW.y1 - PADDING + 0.5,
        "{rect:?}"
    );
}

/// Taller than the room it has, a menu scrolls its rows and ends with the last one it shows
/// whole: a row cut to a sliver at its edge is a target a person sees but cannot aim at.
#[test]
fn a_menu_that_scrolls_cuts_no_row_in_half() {
    let _settled = crate::motion::testing::settled();
    let mut m = ContextMenu::with_items(
        (0..12)
            .map(|i| MenuItem::action(format!("Row {i}"), || {}))
            .collect(),
    );
    let window = Rect::new(0.0, 0.0, 400.0, 200.0);
    m.open_anchored(Rect::new(10.0, 10.0, 300.0, 30.0), window);
    assert!(m.list.overflows(), "the rest scrolls into view");
    assert!(m.get_rect().y1 <= window.y1 + 0.5, "{:?}", m.get_rect());
    let mut shown = 0;
    for index in 0..12 {
        if let Some(item) = m.item_rect(index) {
            shown += 1;
            assert!(
                (item.height() - m.row_height()).abs() < 0.5,
                "row {index} is cut at the panel's edge: {item:?}"
            );
        }
    }
    assert!(shown >= 1, "the menu shows rows");
}

/// A menu a row opens follows the row it stands against: the same call that
/// opens it places it again wherever the row is now.
#[test]
fn a_reanchored_menu_follows_its_row_to_its_new_box() {
    let _settled = crate::motion::testing::settled();
    let mut m = menu();
    m.open_anchored(row(100.0), WINDOW);
    let first = m.get_rect();
    m.reanchor(row(160.0), WINDOW);
    let moved = m.get_rect();
    assert!(
        (moved.y0 - (first.y0 + 60.0)).abs() < 1.0,
        "the list scrolled the row down 60 px: {first:?} then {moved:?}"
    );
}

/// On a coarse pointer the row is itself the target, so it grows to the
/// toolkit's minimum and the panel with it.
#[test]
fn a_menu_row_meets_the_minimum_target_on_a_touch_screen() {
    let _settled = crate::motion::testing::settled();
    let mut m = menu();
    m.open_anchored(row(100.0), WINDOW);
    let fine = m.row_height();
    let touch = crate::interaction::InteractionEnvironment::TOUCH;
    m.set_interaction_environment(touch);
    let target = f64::from(touch.minimum_target());
    assert!(m.row_height() >= target, "{} < {target}", m.row_height());
    assert!(m.row_height() > fine);
    let item = m.item_rect(0).expect("the first row is laid out");
    assert!(item.height() >= target - 0.5, "{item:?}");
}

/// Whatever a row's edges are, the panel's edges land on the display's
/// pixels: the frame draws the menu where it placed it.
#[test]
fn menu_edges_land_on_device_pixels() {
    for scale in [1.5_f64, 2.0, 3.0] {
        let _grid = crate::widget::layout::enter_device_scale(scale);
        let _settled = crate::motion::testing::settled();
        let mut m = ContextMenu::with_items(vec![
            MenuItem::action("Rename this task", || {}),
            MenuItem::action("Duplicate", || {}),
        ]);
        m.set_text_scale(1.5);
        m.open_anchored(Rect::new(25.3, 100.7, 505.6, 139.4), WINDOW);
        let rect = m.get_rect();
        for edge in [rect.x0, rect.y0, rect.x1, rect.y1] {
            let pixels = edge * scale;
            assert!(
                (pixels - pixels.round()).abs() < 1e-6,
                "{edge} is not a device pixel at {scale}x: {rect:?}"
            );
        }
    }
}
