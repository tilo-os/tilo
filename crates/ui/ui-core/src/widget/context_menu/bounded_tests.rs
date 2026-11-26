use super::*;
use crate::synthetic_input::{mouse_press, mouse_release, named_key};
use crate::testing::dispatch;

fn cursor_at(p: Point) -> WindowEvent {
    crate::synthetic_input::cursor_moved(p.x, p.y)
}

fn wheel_lines(y: f32) -> WindowEvent {
    crate::synthetic_input::mouse_wheel(0.0, y)
}

tilo_test_env::scenario! {
    /// A bounded menu measures, scrolls and clamps in every viewport, keeping
    /// every row a person can reach reachable.
    fn bounded_menu_measures_and_clamps_in_every_viewport;


    phase tiny_viewport_keeps_focused_rows_reachable;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
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

    // A bounded menu scrolls by wheel, hit-tests and answers the keyboard on
    // one scroll offset: what the pointer reaches and what the keyboard
    // reaches are the same row.
    phase bounded_wheel_hit_testing_and_keyboard_activation_share_scroll_offset;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut menu = ContextMenu::new();
        for index in 0..20 {
            menu.push(MenuItem::action(format!("Command {index}"), || {}));
        }
        menu.open_anchored(
            Rect::new(10.0, 10.0, 30.0, 30.0),
            Rect::new(0.0, 0.0, 180.0, 100.0),
        );
        menu.layout_within(&mut TextRenderer::new(), &Theme::dark());
        let point = menu.get_rect().center();
        dispatch(&mut menu, &cursor_at(point));
        let wheel = |y| crate::synthetic_input::mouse_wheel(0.0, y);
        assert!(dispatch(&mut menu, &wheel(-100.0)));
        assert_eq!(menu.scroll_offset(), menu.list.max_scroll_offset());
        let last = menu.item_rect(19).unwrap().center();
        assert_eq!(menu.item_at([last.x, last.y]), Some(19));
        dispatch(&mut menu, &cursor_at(last));
        dispatch(&mut menu, &mouse_press());
        dispatch(&mut menu, &mouse_release());
        assert_eq!(menu.take_selection(), Some(19));
        assert_eq!(menu.take_selection(), None);
        menu.open_anchored(Rect::ZERO, Rect::new(0.0, 0.0, 180.0, 100.0));
        menu.focus_first_action();
        for _ in 0..19 {
            dispatch(&mut menu, &named_key(NamedKey::ArrowDown));
        }
        assert_eq!(menu.hovered(), Some(19));
        assert!(menu.item_rect(19).is_some());
        dispatch(&mut menu, &named_key(NamedKey::Enter));
        assert_eq!(menu.take_selection(), Some(19));
        menu.open_anchored(Rect::ZERO, Rect::new(0.0, 0.0, 180.0, 100.0));
        dispatch(&mut menu, &wheel(100.0));
        assert_eq!(menu.scroll_offset(), 0.0);
    }

    phase wheel_scrolls_rows_inside_a_bounded_menu;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = crate::motion::testing::settled();
        let mut menu = ContextMenu::new();
        for label in ["First", "Second", "Third", "Fourth"] {
            menu.push(MenuItem::action(label, || {}));
        }
        let viewport = Rect::new(0.0, 0.0, menu.menu_width(), 68.0);
        menu.set_viewport(Some(viewport));
        menu.open_at([0.0, 0.0]);
        let pointer = Point::new(
            16.0,
            menu.rect.y0 + menu.padding() + menu.row_height() / 2.0,
        );
        dispatch(&mut menu, &cursor_at(pointer));
        assert!(dispatch(&mut menu, &wheel_lines(-2.0)));
        assert_eq!(menu.item_at([pointer.x, pointer.y]), Some(2));
    }

}
