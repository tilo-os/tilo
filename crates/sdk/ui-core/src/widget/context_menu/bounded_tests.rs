use super::*;

#[test]
fn bounded_menu_measures_and_clamps_in_every_viewport() {
    fn bounded_menu_measures_scales_and_clamps_at_all_edges() {
        let viewport = Rect::new(10.0, 20.0, 110.0, 95.0);
        for anchor in [
            Rect::new(10.0, 20.0, 30.0, 40.0),
            Rect::new(90.0, 20.0, 110.0, 40.0),
            Rect::new(10.0, 75.0, 30.0, 95.0),
            Rect::new(90.0, 75.0, 110.0, 95.0),
        ] {
            let mut menu = ContextMenu::new();
            for _ in 0..8 {
                menu.push(MenuItem::action(
                    "Sehr lange übersetzte Aktion 共有 العربية",
                    || {},
                ));
            }
            menu.open_anchored(anchor, viewport);
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            menu.layout_within(&mut text, &Theme::dark().with_text_scale(2.0));
            assert_eq!(menu.get_rect().intersect(viewport), menu.get_rect());
            assert!(menu.row_height > ROW_HEIGHT);
            menu.focus_item(7);
            assert!(menu.scroll_offset() > 0.0);
            let last = menu.item_rect(7).unwrap();
            assert_eq!(menu.item_at([last.center().x, last.center().y]), Some(7));
            assert!(menu.item_rect(0).is_none());
            menu.move_hover(1);
            assert_eq!(menu.hovered(), Some(0));
            assert_eq!(menu.scroll_offset(), 0.0);
        }
    }

    fn width_uses_shaping_and_legacy_open_resets_bounded_geometry() {
        let mut narrow = ContextMenu::new();
        narrow.push(MenuItem::action("i".repeat(40), || {}));
        let mut wide = ContextMenu::new();
        wide.push(MenuItem::action("W".repeat(40), || {}));
        let mut text = TextRenderer::new();
        for menu in [&mut narrow, &mut wide] {
            menu.open_anchored(Rect::ZERO, Rect::new(0.0, 0.0, 1000.0, 1000.0));
            menu.layout_within(&mut text, &Theme::dark());
        }
        assert!(wide.get_rect().width() > narrow.get_rect().width());
        // However short its one label, a menu keeps a menu's width.
        let min = f64::from(Theme::dark().tokens.metrics.menu_min_width);
        assert!(narrow.get_rect().width() >= min - 0.5);
        wide.open_at([900.0, 950.0]);
        assert_eq!(
            wide.get_rect(),
            Rect::new(
                900.0,
                950.0,
                900.0 + MENU_WIDTH,
                950.0 + ROW_HEIGHT + 2.0 * PADDING
            )
        );
        assert_eq!(wide.scroll_offset(), 0.0);
    }

    fn tiny_viewport_keeps_focused_rows_reachable() {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("First", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Last", || {}));
        let viewport = Rect::new(0.0, 0.0, 4.0, 4.0);
        menu.open_anchored(viewport, viewport);
        menu.layout_within(&mut TextRenderer::new(), &Theme::dark());
        menu.focus_item(2);
        assert_eq!(menu.get_rect(), viewport);
        let last = menu.item_rect(2).unwrap();
        assert_eq!(menu.item_at([last.center().x, last.center().y]), Some(2));
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    bounded_menu_measures_scales_and_clamps_at_all_edges();
    width_uses_shaping_and_legacy_open_resets_bounded_geometry();
    tiny_viewport_keeps_focused_rows_reachable();
}
