//! The window's own controls at the trailing end of its bar, and the app's
//! own actions that stand beside them: what belongs to the window, and what
//! belongs to what the window shows.

use super::tests::{
    bar_with_laid_out_rects, browser_parts, holder, lay_out_at_width_with, move_to,
};
use super::*;
use crate::testing::dispatch;
use winit::event::{ElementState, MouseButton};

tilo_test_env::scenario! {
    /// A window bar's controls: where they stand, what they report and what a press does.
    /// Each phase was a test of its own and builds its own fixture.
    fn window_controls_stand_report_and_close;

    phase a_held_window_offers_no_close;
    // A window someone else holds, or its person only watches, offers no
    // close among its controls, to the pointer or to assistive technology,
    // and keeps the rest.
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = tilo_ui_base::theme::Theme::dark().with_reduce_motion(true);
        let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "Notes")], 1);
        b.set_window_controls(true);
        b.set_holder(
            1,
            Some(TabHolder {
                name: "Lounge".into(),
                rgb: (1, 2, 3),
                keeps: true,
            }),
        );
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let controls = *b.window_controls();
        assert!(controls.close.area() <= 0.0);
        assert!(controls.maximize.area() > 0.0);
        let node = Widget::accessibility(&b).expect("the bar is described");
        fn targets(node: &tilo_ui_base::a11y::AccessNode, into: &mut Vec<String>) {
            into.extend(node.action_target.clone());
            for child in &node.children {
                targets(child, into);
            }
        }
        let mut all = Vec::new();
        targets(&node, &mut all);
        assert!(all.iter().any(|t| t == WINDOW_MAXIMIZE_TARGET), "{all:?}");
        assert!(!all.iter().any(|t| t == WINDOW_CLOSE_TARGET), "{all:?}");
        assert!(!all.iter().any(|t| t == "tab-close:1"), "{all:?}");
        assert!(all.iter().any(|t| t == "tab-new"), "{all:?}");
        assert!(b.set_new_tab(false));
        let mut all = Vec::new();
        targets(&Widget::accessibility(&b).expect("described"), &mut all);
        assert!(!all.iter().any(|t| t == "tab-new"), "{all:?}");
        b.set_holder(1, None);
        lay_out_at_width_with(&mut b, 900.0, &theme);
        assert!(
            b.window_controls().close.area() > 0.0,
            "its own window closes"
        );
    }




    phase a_middle_press_closes_the_tab_under_it;
    {
        let mut b = bar_with_laid_out_rects();
        move_to(&mut b, 95.0, 20.0);
        let press = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Middle,
        };
        assert!(dispatch(&mut b, &press), "the bar takes the middle press");
        assert_eq!(b.pending_close, Some(2), "closing pill B asks to close B");
        assert!(b.closing_tabs.iter().any(|c| c.desc.id == 2));
        assert_eq!(b.active_id, 1, "another tab closing leaves the active one");

        // A tab someone else holds is not this person's to close, by the ×
        // or by a middle press.
        let mut b = bar_with_laid_out_rects();
        b.set_holder(2, Some(holder()));
        move_to(&mut b, 95.0, 20.0);
        dispatch(&mut b, &press);
        assert_eq!(b.pending_close, None);
        assert!(b.closing_tabs.is_empty());

        let mut b = bar_with_laid_out_rects();
        b.pin(2);
        move_to(&mut b, 95.0, 20.0);
        dispatch(&mut b, &press);
        assert_eq!(b.pending_close, None);

        let mut b = solo_bar_with_slots();
        assert!(b.icon_only(1));
        b.tab_rects = vec![Rect::new(0.0, 6.0, 40.0, 34.0)];
        move_to(&mut b, 20.0, 20.0);
        assert!(dispatch(&mut b, &press));
        assert_eq!(b.pending_close, Some(1), "the window's only tab closes");
    }

    phase a_secondary_press_on_the_location_opens_the_windows_menu;
    {
        let mut b = solo_bar_with_slots();
        lay_out_at_width_with(&mut b, 900.0, &tilo_ui_base::theme::Theme::dark());
        let location = b.location_rect();
        assert!(
            location.area() > 0.0,
            "the app's location stands in the row"
        );
        let reload = b.toolbar_rects().3[0];
        let right = tilo_ui_base::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right);
        move_to(&mut b, location.center().x, location.center().y);
        assert!(dispatch(&mut b, &right), "the bar takes the press");
        assert_eq!(
            b.take_context_menu().map(|(tab, _)| tab),
            Some(1),
            "the location opens its window's menu"
        );
        move_to(&mut b, reload.center().x, reload.center().y);
        dispatch(&mut b, &right);
        assert_eq!(b.take_context_menu(), None, "the app's control keeps it");

        // Two tabs put the slots on a row of their own under the tabs: still the window's bar.
        let mut b = TabBar::new(
            "g1",
            vec![TabDescriptor::new(1, "Web"), TabDescriptor::new(2, "Notes")],
            1,
        );
        b.set_window_controls(true);
        assert!(b.set_toolbar(1, Some(browser_parts())));
        lay_out_at_width_with(&mut b, 900.0, &tilo_ui_base::theme::Theme::dark());
        let location = b.location_rect();
        assert!(location.y0 >= b.tab_row().y1, "{location:?}");
        move_to(&mut b, location.center().x, location.center().y);
        dispatch(&mut b, &right);
        assert_eq!(b.take_context_menu().map(|(tab, _)| tab), Some(1), "not the workspace's menu");
    }

    phase a_windows_only_tab_shows_the_name_a_person_gave_it;
    {
        let mut b = solo_bar_with_slots();
        b.tab_rects = vec![Rect::new(0.0, 6.0, 40.0, 34.0)];
        assert!(b.icon_only(1));
        assert!(
            b.begin_rename(1, "Window name"),
            "the icon opens to a field"
        );
        assert!(!b.icon_only(1), "the field stands where the title would");
        assert!(b.cancel_rename());
        assert!(b.icon_only(1), "Escape leaves the icon");
        let mut named = TabDescriptor::new(1, "Plan");
        named.named = true;
        b.tabs[0] = named;
        assert!(
            !b.icon_only(1),
            "a named tab shows its name beside the location"
        );
        b.pin(1);
        assert!(
            !b.begin_rename(1, "Window name"),
            "a pinned tab has no title to edit"
        );
    }



}

fn solo_bar_with_slots() -> TabBar {
    let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "DuckDuckGo")], 1);
    b.set_window_controls(true);
    assert!(b.set_toolbar(1, Some(browser_parts())));
    b
}
