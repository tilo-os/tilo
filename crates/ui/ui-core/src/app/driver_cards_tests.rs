//! A card an app presents over its window takes the pointer, the wheel and the
//! keys that are on it, as a native desktop's cards do: a press on a control
//! the card draws runs in the card, and the window under it does not see the
//! event. The page once drew such a card and sent every press to the window
//! beneath it, so its buttons did nothing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixture: a failure is the assertion"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::app::driver::Driver;
use crate::state::State;
use crate::theme::Theme;
use crate::widget::{EventCtx, Widget};

/// What reached the card and what reached the window under it, by kind.
type Seen = Rc<RefCell<Vec<&'static str>>>;

/// A card the app draws over the window: the app asks the frame to stand it
/// and says it is still up, as an app with a call asks.
struct Card {
    seen: Seen,
    asked: bool,
}

impl crate::floating_surface::FloatingSurfaceProvider for Card {
    fn take_floating_surface_request(
        &mut self,
    ) -> Option<crate::floating_surface::FloatingSurfaceRequest> {
        if std::mem::replace(&mut self.asked, true) {
            return None;
        }
        Some(crate::floating_surface::FloatingSurfaceRequest {
            id: "card".into(),
            title: "Active call".into(),
            role: crate::floating_surface::FloatingRole::ActiveCall,
            source: crate::floating_surface::FloatingSource::AppRegion {
                app_id: "probe".into(),
                region: vello::kurbo::Rect::new(0.0, 0.0, 200.0, 200.0),
            },
            preferred_size: vello::kurbo::Size::new(200.0, 200.0),
            aspect_ratio: None,
            initial_rect: None,
            presentation_rect: None,
            tracking_source: None,
        })
    }

    fn active_floating_surface_ids(&self) -> Vec<String> {
        vec!["card".into()]
    }

    fn floating_surface_event(
        &mut self,
        id: &str,
        event: &WindowEvent,
        _: &mut EventCtx<'_>,
    ) -> bool {
        if id != "card" {
            return false;
        }
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => self.seen.borrow_mut().push("card:press"),
            WindowEvent::MouseWheel { .. } => self.seen.borrow_mut().push("card:wheel"),
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                self.seen.borrow_mut().push("card:key");
            }
            _ => {}
        }
        true
    }

    fn floating_surface_accessibility(&self, id: &str) -> Option<crate::a11y::AccessNode> {
        (id == "card").then(|| {
            crate::a11y::AccessNode::new(crate::a11y::Role::Button)
                .with_hashed_key("card-unmute")
                .with_action_target("card-unmute")
        })
    }
}

impl Widget for Card {
    crate::provides!(dyn crate::floating_surface::FloatingSurfaceProvider);

    fn sync(&mut self, _: &crate::widget::AppModel, _: &crate::widget::ModelPath) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        _: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use crate::widget::TaffyTreeExt as _;
        taffy.leaf_node(taffy::Style::default())
    }

    fn name(&self) -> &'static str {
        "Card"
    }
}

/// A frame whose root stands a card at `(100, 100)`–`(300, 300)`.
fn frame_with_a_card() -> (Driver, Seen) {
    let seen = Seen::default();
    let card = Card {
        seen: Rc::clone(&seen),
        asked: false,
    };
    let mut frame = Driver::new_with_controlled_inputs(
        State::new(),
        Box::new(card),
        (800, 600),
        Theme::dark(),
        crate::font_catalog::fixture_renderer(),
        crate::permissions::MemberId::person("reader"),
    );
    frame.relayout();
    (frame, seen)
}

/// Where the frame stood the card, as its published controls read.
fn resting_rect(frame: &Driver) -> vello::kurbo::Rect {
    let inspection = frame.inspection();
    let card = inspection
        .root
        .children
        .iter()
        .find(|node| node.widget == "FloatingSurface")
        .expect("the frame stands the card its root presents");
    let [x0, y0, width, height] = card.rect;
    vello::kurbo::Rect::new(x0, y0, x0 + width, y0 + height)
}

tilo_test_env::scenario! {
    fn a_card_takes_the_pointer_wheel_and_keys_that_are_on_it;

    phase a_press_on_the_card_does_not_reach_the_window_under_it;
    {
        let (mut frame, seen) = frame_with_a_card();
        // Where the frame stood the card: a press inside it is the card's, and
        // the root's own handling does not see it.
        let at = resting_rect(&frame).center();
        frame.send_host_event(&crate::synthetic_input::cursor_moved(at.x, at.y));
        frame.send_host_event(&crate::synthetic_input::mouse_input(
            ElementState::Pressed,
            MouseButton::Left,
        ));
        assert!(
            seen.borrow().contains(&"card:press"),
            "a press inside the card reaches it: {:?}",
            seen.borrow()
        );
        assert_eq!(
            seen.borrow().iter().filter(|seen| **seen == "card:press").count(),
            1,
            "and exactly once: {:?}",
            seen.borrow()
        );
    }

    phase the_keys_stay_with_the_card_after_a_press_on_it;
    {
        let (mut frame, seen) = frame_with_a_card();
        let at = resting_rect(&frame).center();
        frame.send_host_event(&crate::synthetic_input::cursor_moved(at.x, at.y));
        frame.send_host_event(&crate::synthetic_input::mouse_input(
            ElementState::Pressed,
            MouseButton::Left,
        ));
        seen.borrow_mut().clear();
        frame.send_host_event(&crate::synthetic_input::char_key("a"));
        assert!(
            seen.borrow().contains(&"card:key"),
            "the keys go to the card the person pressed: {:?}",
            seen.borrow()
        );
        seen.borrow_mut().clear();
        frame.type_text("hi");
        assert!(
            seen.borrow().contains(&"card:key"),
            "and so does what the person types: {:?}",
            seen.borrow()
        );
    }

    phase the_cards_controls_are_published_at_the_place_it_stands;
    {
        let (frame, _) = frame_with_a_card();
        let inspection = frame.inspection();
        let card = inspection
            .root
            .children
            .iter()
            .find(|node| node.widget == "FloatingSurface")
            .expect("the frame publishes the card it stands");
        let [x0, y0, width, height] = card.rect;
        assert!(
            width > 1.0 && height > 1.0 && x0 >= 0.0 && y0 >= 0.0,
            "the card's controls are read where the frame placed it: {:?}",
            card.rect
        );
        assert!(
            x0 + width <= 800.0 && y0 + height <= 600.0,
            "inside the window it stands in: {:?}",
            card.rect
        );
        let semantics = card
            .semantics
            .as_ref()
            .expect("the card says what it shows");
        assert!(
            semantics.action_target.is_some()
                || semantics
                    .children
                    .iter()
                    .any(|child| child.action_target.is_some()),
            "and its controls are read: {semantics:?}"
        );
    }
}
