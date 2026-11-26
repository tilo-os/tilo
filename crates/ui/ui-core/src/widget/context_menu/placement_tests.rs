//! One placement for every way a menu opens: the control, row or point it
//! stands against decides where its panel lands, whether it fits below, and
//! which pixels of the display its edges fall on.

use super::*;
use crate::presence::PresenceCtx;

const WINDOW: Rect = Rect::new(0.0, 0.0, 800.0, 600.0);

fn menu() -> ContextMenu {
    ContextMenu::with_items(vec![
        MenuItem::action("Rename", || {}),
        MenuItem::action("Duplicate", || {}),
        MenuItem::action("Delete", || {}),
    ])
}

/// The row "Find the last service date" in the reported capture: a wide row
/// with a gap below it.
fn row(y0: f64) -> Rect {
    Rect::new(25.0, y0, 505.0, y0 + 39.0)
}

tilo_test_env::scenario! {
    /// Where a menu stands: under its row, at the pointer, flipped, clamped, scrolled and on device pixels.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_menu_stands_where_its_opening_put_it_inside_the_window;

    // Opened from a control, the menu hangs one anchor gap under it with its
    // leading edge on the control's; at the window's trailing edge it lines up
    // with the control's trailing edge, and near the bottom it opens above.
    phase a_menu_opened_from_the_keyboard_stands_under_its_row;
    {
        let _settled = crate::motion::testing::settled();
        let mut m = menu();
        let anchor = row(100.0);
        m.open_anchored(anchor, WINDOW);
        let rect = m.get_rect();
        assert!(
            rect.y0 >= anchor.y1 && rect.y0 - anchor.y1 <= ANCHOR_GAP + 0.5,
            "directly under the row: {rect:?} vs {anchor:?}"
        );
        assert!(
            (rect.x0 - anchor.x0).abs() < 1.0,
            "hugging its leading edge: {rect:?} vs {anchor:?}"
        );
        let button = Rect::new(300.0, 16.0, 332.0, 48.0);
        let mut m = menu();
        m.open_anchored(button, WINDOW);
        m.layout_within(&mut TextRenderer::new(), &Theme::dark());
        let rect = m.get_rect();
        assert!((rect.x0 - button.x0).abs() < 1e-9, "{rect:?}");
        assert!(
            (rect.y0 - (button.y1 + ANCHOR_GAP)).abs() < 1e-9,
            "{rect:?}"
        );

        let more = Rect::new(736.0, 16.0, 768.0, 48.0);
        let mut m = menu();
        m.open_anchored(more, WINDOW);
        m.layout_within(&mut TextRenderer::new(), &Theme::dark());
        let rect = m.get_rect();
        assert!(
            (rect.x1 - more.x1).abs() < 1e-9,
            "trailing-aligned: {rect:?}"
        );
        assert!(rect.y0 >= more.y1, "under its trigger: {rect:?}");

        let low = row(540.0);
        let mut m = menu();
        m.open_anchored(low, WINDOW);
        let rect = m.get_rect();
        assert!(
            rect.y1 <= low.y0 && low.y0 - rect.y1 <= ANCHOR_GAP + 0.5,
            "above the row where below is too small: {rect:?} vs {low:?}"
        );
    }

    phase a_menu_opened_at_the_pointer_stands_at_the_pointer_and_stays_in_the_window;
    {
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

        // The same point-placed menu painted as an overlay at the window's
        // bottom-trailing corner stays in the window it is presented in.
        let mut m = menu();
        m.open_at([1270.0, 790.0]);
        let presence = PresenceCtx::default();
        presence.viewport.set(Some(Rect::new(0.0, 0.0, 1280.0, 800.0)));
        let mut scene = Scene::new();
        m.draw_overlay(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut TextRenderer::new(), &Theme::dark(), presence.draw_env()));
        let rect = m.get_rect();
        assert!(
            rect.x0 >= 0.0 && rect.x1 <= 1280.0 && rect.y0 >= 0.0 && rect.y1 <= 800.0,
            "a menu at the bottom-trailing corner stays in the window: {rect:?}"
        );
    }

    // Taller than the height it has, a menu scrolls its rows and ends with the last one it shows
    // whole: a row cut to a sliver at its edge is a target a person sees but cannot aim at.
    phase a_menu_that_scrolls_cuts_no_row_in_half;
    {
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


    // A menu a row opens follows the row it stands against: the same call that
    // opens it places it again wherever the row is now.
    phase a_reanchored_menu_follows_its_row_to_its_new_box;
    {
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


}
