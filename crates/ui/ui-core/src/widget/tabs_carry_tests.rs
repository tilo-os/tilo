//! The bar lifts the window it belongs to: a window of one tab shows it as the bar's title, so
//! the whole bar is its handle; a bar of several keeps its presses for its tabs.

#![allow(
    clippy::unwrap_used,
    reason = "test fixture: failures are the assertion"
)]

use super::tests::{crowded_parts, dispatch_cursor, dispatch_left_button, lay_out_at_width_with};
use super::{TabBar, TabDescriptor};
use vello::kurbo::Vec2;
use winit::event::ElementState;

tilo_test_env::scenario! {
    fn a_bar_lifts_the_window_it_belongs_to;

    phase a_bar_of_one_tab_is_grabbed_by_its_title_and_location_and_clicks_still_work;
    // A window of one tab shows it as the bar's title, so the bar lifts the window from its title
    // and its location wherever the window stands; the press that travels is no click.
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = tilo_ui_base::theme::Theme::dark().with_reduce_motion(true);
        let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "DuckDuckGo")], 1);
        b.set_window_controls(true);
        b.set_toolbar(1, Some(crowded_parts()));
        lay_out_at_width_with(&mut b, 900.0, &theme);
        assert!(!b.carriable, "fixture: nothing carries the bar from anywhere");
        assert!(b.lifts_the_window(), "one tab is the window's title");
        let (_, _, location, _) = b.toolbar_rects();
        for at in [location.center(), b.tab_rects[0].center()] {
            let mut handled = false;
            dispatch_cursor(&mut b, at, &mut handled);
            dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
            assert!(!b.editing_location(), "nothing happens until the press ends");
            dispatch_cursor(&mut b, at + Vec2::new(40.0, 30.0), &mut handled);
            assert!(!b.drag_in_flight(), "the window is carried, not its tab");
            assert!(!b.editing_location(), "a carry is no click");
            b.cancel_press_for_carry();
            dispatch_left_button(&mut b, ElementState::Released, &mut handled);
            assert!(!b.editing_location(), "the press ended as a carry");
        }

        // A plain click still edits the location, as it does on a bar of several.
        let mut handled = false;
        dispatch_cursor(&mut b, location.center(), &mut handled);
        dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
        dispatch_left_button(&mut b, ElementState::Released, &mut handled);
        assert!(b.editing_location(), "a click edits the location");
    }

    phase a_bar_of_several_tabs_keeps_its_presses_for_the_tabs;
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = tilo_ui_base::theme::Theme::dark().with_reduce_motion(true);
        let mut b = TabBar::new(
            "g1",
            vec![TabDescriptor::new(1, "A"), TabDescriptor::new(2, "B")],
            1,
        );
        b.set_window_controls(true);
        b.set_toolbar(1, Some(crowded_parts()));
        lay_out_at_width_with(&mut b, 900.0, &theme);
        assert!(!b.lifts_the_window(), "two tabs are a bar of tabs");
        let title = b.tab_rects[0].center();
        let mut handled = false;
        dispatch_cursor(&mut b, title, &mut handled);
        dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
        dispatch_cursor(&mut b, title + Vec2::new(40.0, 30.0), &mut handled);
        assert!(b.drag_in_flight(), "a press on a tab of several lifts the tab");
        b.cancel_drag();
    }

    phase a_bar_of_several_tabs_keeps_its_presses_while_the_window_can_be_carried;
    // A bar that carries the window (its frame hands another window what the window holds) still
    // keeps its pills for its tabs: the window is carried by the bar's own room, and a person who
    // presses a pill carries the tab, never the window.
    {
        let _settled = tilo_ui_base::motion::testing::settled();
        let theme = tilo_ui_base::theme::Theme::dark().with_reduce_motion(true);
        let mut b = TabBar::new(
            "g1",
            vec![TabDescriptor::new(1, "A"), TabDescriptor::new(2, "B")],
            1,
        );
        b.set_window_controls(true);
        b.set_toolbar(1, Some(crowded_parts()));
        b.set_carriable(true);
        lay_out_at_width_with(&mut b, 900.0, &theme);
        assert!(b.carriable, "fixture: the bar carries the window");
        assert!(!b.lifts_the_window(), "two tabs are a bar of tabs");
        let title = b.tab_rects[0].center();
        let mut handled = false;
        dispatch_cursor(&mut b, title, &mut handled);
        dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
        dispatch_cursor(&mut b, title + Vec2::new(40.0, 30.0), &mut handled);
        assert!(
            b.drag_in_flight(),
            "a press on a tab of several carries the tab, even where the bar carries the window"
        );
        b.cancel_drag();
    }
}
