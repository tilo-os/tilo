//! A phone's window bar: what stands on it, and in which order — the strip's own "+", the app's
//! Back and Forward, the name the window goes by, and the actions beside them — so no two controls
//! share a place at any width.

use super::tests::{browser_slots, lay_out_at_width_with};
use super::*;
use crate::widget::window_toolbar_row::MIN_LOCATION_PX;

/// A window of one tab names itself in the bar's location slot, and the strip's own "+" stands
/// beside that name. Neither paints over the other: the title used to start under the control,
/// so the two strings were drawn on top of each other ("New tab" across "Notes").
#[test]
fn a_windows_title_stands_clear_of_the_new_tab_control() {
    use crate::window_toolbar::{ToolbarLocation, WindowToolbar};
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::dark().with_reduce_motion(true);
    for width in [1280.0, 390.0] {
        let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "Notes")], 1);
        b.set_window_controls(true);
        // An app that declares a location of its own but names nothing yet: the bar fills the
        // name in from the tab (`show_toolbar_row`), so this is where the window's title stands.
        b.set_toolbar(
            1,
            Some(WindowToolbar {
                location: Some(ToolbarLocation::default()),
                ..WindowToolbar::default()
            }),
        );
        lay_out_at_width_with(&mut b, width, &theme);
        let plus = b.new_tab_rect();
        let title = b.location_rect();
        assert!(plus.area() > 0.0, "{width}: the strip offers its +");
        assert!(title.area() > 0.0, "{width}: the window names itself here");
        assert!(
            title.x0 >= plus.x1,
            "{width}: the title starts past the + ({title:?} over {plus:?})"
        );
    }
}

/// A phone's window bar gives each of its controls its own place: the strip's own "+", the
/// app's Back and Forward, and the name the window goes by stand in that order, none painted
/// over another, and all of them inside the bar.
#[test]
fn a_phone_bar_keeps_each_control_in_its_own_place() {
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::light().with_reduce_motion(true);
    let mut b = TabBar::new("g1", vec![TabDescriptor::new(1, "Notes")], 1);
    b.set_window_controls(true);
    b.set_toolbar(1, Some(browser_slots()));
    lay_out_at_width_with(&mut b, 390.0, &theme);
    let bar = b.get_rect();
    let plus = b.new_tab_rect();
    let (back, forward, title, actions) = b.toolbar_rects();
    assert!(plus.area() > 0.0, "the strip offers its +");
    assert!(back.area() > 0.0, "the app offers Back");
    assert!(title.area() > 0.0, "the window names itself");
    assert!(
        plus.x1 <= back.x0,
        "the + ends before Back: {plus:?} {back:?}"
    );
    assert!(
        back.x1 <= forward.x0,
        "Back ends before Forward: {back:?} {forward:?}"
    );
    assert!(
        forward.x1 <= title.x0,
        "Forward ends before the title: {forward:?} {title:?}"
    );
    if let Some(first) = actions.first() {
        assert!(
            title.x1 <= first.x0,
            "the title ends before the app's actions: {title:?} {first:?}"
        );
    }
    for rect in [plus, back, forward, title] {
        assert!(
            bar.x0 <= rect.x0 && rect.x1 <= bar.x1 + 0.5,
            "{rect:?} stands inside the bar {bar:?}"
        );
    }
    assert!(
        title.width() >= MIN_LOCATION_PX,
        "the title keeps room to be read: {title:?}"
    );
}
