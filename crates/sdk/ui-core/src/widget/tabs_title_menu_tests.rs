//! Slots that are a menu alone, in the title's row.

use super::super::tests::{dispatch_cursor, dispatch_left_button, lay_out_at_width_with, send_key};
use super::super::{PINNED_W, TAB_BAR_HEIGHT, TabBar, TabDescriptor};
use crate::widget::Widget as _;
use crate::window_toolbar::{ToolbarAction, ToolbarEvent, WindowToolbar};
use vello::kurbo::Rect;
use winit::event::ElementState;
use winit::keyboard::{Key, NamedKey};

/// Slots that are a menu alone (the desktop's own items on the window of an
/// app that declared no bar) add no row and move no title: the "More
/// actions" button stands at the end of the tabs' row, before the window's
/// controls, and its menu runs the item.
#[test]
fn a_menu_alone_is_a_button_in_the_title_row_that_keeps_the_title() {
    let _settled = crate::motion::testing::settled();
    let theme = crate::theme::Theme::dark().with_reduce_motion(true);
    let menu = WindowToolbar {
        actions: vec![ToolbarAction {
            menu: true,
            ..ToolbarAction::new(
                "tilo.desktop.share-to-channel",
                "screen-share",
                "Share to channel…",
            )
        }],
        ..WindowToolbar::default()
    };
    let lookup = crate::i18n::I18nManager::lookup_plain;
    for tabs in [1_u64, 2] {
        let mut b = TabBar::new(
            "g1",
            (1..=tabs)
                .map(|id| TabDescriptor::new(id, format!("Notes {id}")))
                .collect(),
            1,
        );
        b.set_window_controls(true);
        lay_out_at_width_with(&mut b, 900.0, &theme);
        let bare_pill = b.tab_rects()[0];
        b.set_toolbar(1, Some(menu.clone()));
        lay_out_at_width_with(&mut b, 900.0, &theme);
        assert!(
            (b.bar_height() - TAB_BAR_HEIGHT).abs() < f64::EPSILON,
            "{tabs} tab(s): no row of its own"
        );
        let more = b.toolbar_row().slots().overflow;
        assert!(more.area() > 0.0, "{tabs} tab(s): the menu button stands");
        assert!(more.x1 <= b.window_controls().maximize.x0);
        assert!(more.y1 <= b.rect.y0 + TAB_BAR_HEIGHT, "in the tabs' row");
        let pills_end = b.tab_rects().iter().map(|r| r.x1).fold(0.0, f64::max);
        assert!(
            pills_end <= more.x0,
            "{tabs} tab(s): the tabs end before it"
        );
        assert_eq!(b.location_rect(), Rect::ZERO, "no location");
        assert!(
            b.tab_rects()[0].width() > PINNED_W + 0.5,
            "{tabs} tab(s): the title stays in its tab"
        );
        assert!((b.tab_rects()[0].x0 - bare_pill.x0).abs() < 0.5);
        assert_eq!(
            b.tooltip_target_at(more.center()).map(|(_, t)| t),
            Some(lookup("window-toolbar-more"))
        );

        b.set_popup_area(Rect::new(0.0, 0.0, 900.0, 700.0));
        let mut handled = false;
        dispatch_cursor(&mut b, more.center(), &mut handled);
        dispatch_left_button(&mut b, ElementState::Pressed, &mut handled);
        dispatch_left_button(&mut b, ElementState::Released, &mut handled);
        assert!(b.toolbar_row().popup_open(), "the menu opened");
        assert!(send_key(&mut b, Key::Named(NamedKey::ArrowDown), None));
        assert!(send_key(&mut b, Key::Named(NamedKey::Enter), None));
        assert_eq!(
            b.take_toolbar_events(),
            vec![(
                1,
                ToolbarEvent::Action {
                    id: "tilo.desktop.share-to-channel".into()
                }
            )]
        );
    }
}
