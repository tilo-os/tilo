//! A window arriving, maximizing and leaving: its place opens at once (its content's arrival inside
//! it is `window_group_arrival_tests`), a new tab keeps the tab before it until it has drawn, a
//! springing window is moved and clipped but never stretched, and a key closes at once.
use super::tests::make_group;
use super::*;
use crate::widget::{AppKind, TabDescriptor, TileContent};
use std::collections::HashMap;

struct Arrives {
    drawn: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Widget for Arrives {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _t: &tilo_ui_base::theme::Theme,
        _m: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.leaf_node(taffy::Style::default())
    }
    fn name(&self) -> &'static str {
        "Arrives"
    }
    fn presented(&self) -> bool {
        self.drawn.get()
    }
}

impl crate::widget::app_widget::AppWidget for Arrives {
    fn app_kind(&self) -> AppKind {
        AppKind::Files
    }
    fn app_title(&self) -> &'static str {
        "Arrives"
    }
    fn app_icon(&self) -> crate::widget::app_widget::IconDescriptor {
        crate::widget::app_widget::IconDescriptor::Glyph("A")
    }
}

/// A new tab (Super+T) never blanks its window: the tab shown before
/// stays until the new one has drawn, then the two cross-fade.
#[test]
fn a_new_tab_shows_the_tab_before_it_until_it_draws_then_cross_fades() {
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let theme = Theme::dark();
        let mut g = make_group();
        assert_eq!(g.active_tab_id(), 1);
        let drawn = std::rc::Rc::new(std::cell::Cell::new(false));
        g.insert_detached_tab(
            2,
            TabDescriptor::new(42, "new"),
            TileHost::new(TileContent::App(Box::new(Arrives {
                drawn: std::rc::Rc::clone(&drawn),
            }))),
        );
        assert_eq!(g.active_tab_id(), 42, "the new tab is shown at once");
        g.advance_tab_handover(&theme);
        assert_eq!(
            g.stand_in_tab(),
            Some(1),
            "until it draws, the tab shown before stands in"
        );
        drawn.set(true);
        g.advance_tab_handover(&theme);
        assert_eq!(g.stand_in_tab(), None, "it has drawn");
        let now = tilo_ui_base::motion::frame_now();
        let fade = g.tab_fade.as_ref().expect("the two cross-fade");
        assert_eq!(
            fade.outgoing(now).map(|(id, alpha)| (*id, alpha)),
            Some((1, 1.0)),
            "the cross-fade starts from the old content, whole"
        );
        assert!(fade.incoming(now) < 0.05, "and the new one clear");
        assert!(g.needs_animation_frame(), "the cross-fade asks for frames");
    });
}

#[test]
fn a_key_closes_a_window_at_once_and_a_click_after_its_fade() {
    crate::time::testing::with_monotonic_time(crate::time::now(), || {
        let mut g = make_group();
        g.set_motion_source(tilo_ui_base::tokens::MotionSource::Keyboard);
        g.start_closing();
        assert!(g.close_complete(), "a key's close is complete at once");

        let mut g = make_group();
        g.set_motion_source(tilo_ui_base::tokens::MotionSource::Pointer);
        g.start_closing();
        assert!(!g.close_complete(), "a click's close fades first");
        assert!(
            g.needs_animation_frame(),
            "and asks for the frames of its fade"
        );
    });
}
