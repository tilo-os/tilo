use super::*;
use crate::testing::dispatch;
use std::collections::HashMap;
use tilo_ui_base::{
    synthetic_input::mouse_press, synthetic_input::mouse_release, synthetic_input::named_key,
};

fn cursor_at(p: Point) -> WindowEvent {
    tilo_ui_base::synthetic_input::cursor_moved(p.x, p.y)
}

fn wheel_lines(y: f32) -> WindowEvent {
    tilo_ui_base::synthetic_input::mouse_wheel(0.0, y)
}

/// Lay `menu` out on its own, as a host that paints it does.
fn lay_out(menu: &mut ContextMenu) {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = menu.build_layout(&mut taffy, &theme, &mut node_map);
    taffy
        .compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(800.0),
                height: taffy::AvailableSpace::Definite(600.0),
            },
        )
        .unwrap();
    menu.apply_layout(&taffy, node, Point::ZERO);
}

tilo_test_env::scenario! {
    /// A bounded menu measures, scrolls and clamps in every viewport, keeping
    /// every row a person can reach reachable.
    fn bounded_menu_measures_and_clamps_in_every_viewport;

    phase bounded_menu_measures_scales_and_clamps_at_all_edges;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
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

    phase tiny_viewport_keeps_focused_rows_reachable;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
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
        let _settled = tilo_ui_base::motion::testing::settled();
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
        let wheel = |y| tilo_ui_base::synthetic_input::mouse_wheel(0.0, y);
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
        let _settled = tilo_ui_base::motion::testing::settled();
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

    phase text_scale_updates_layout_and_hit_testing_together;
    {
        // Motion reads at its end: this test checks the resting frame.
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut m = ContextMenu::new();
        m.push(MenuItem::action("Cut", || {}));
        m.push(MenuItem::separator());
        m.push(MenuItem::action("Copy", || {}));
        m.open_at([100.0, 100.0]);
        lay_out(&mut m);
        let before = m.get_rect();
        m.set_text_scale(2.0);
        let after = m.get_rect();
        assert_eq!(after.width(), before.width() * 2.0);
        assert_eq!(
            after.height(),
            PADDING * 2.0 + ROW_HEIGHT * 4.0 + SEPARATOR_HEIGHT
        );

        let second_action = m.item_rect(2).expect("second action row");
        let p = Point::new(
            f64::midpoint(second_action.x0, second_action.x1),
            second_action.y0 + 1.0,
        );
        assert_eq!(m.item_at([p.x, p.y]), Some(2));
    }
}
