//! Direct touch reaches ordinary widgets through the host's touch-to-pointer
//! contract: two fingers pinch or pan, a still hold is a secondary click, and
//! a widget claiming raw touch receives every finger itself.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tilo_ui::widget::TaffyTreeExt;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use tilo_ui::State;
use tilo_ui::headless::Driver;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::Widget;
use tilo_ui::widget::context_menu::{ContextMenu, MenuItem};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};

/// Fills the window, records every event it is given and, like any widget
/// with a context menu, opens its menu on a secondary press.
struct Probe {
    rect: Rect,
    raw_touch: bool,
    cursor: Point,
    menu: ContextMenu,
    events: Rc<RefCell<Vec<WindowEvent>>>,
    menu_open_at: Rc<RefCell<Option<[f32; 2]>>>,
}

impl Widget for Probe {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x, position.y);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.rect.contains(self.cursor) => {
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "on-screen test coordinates"
                )]
                let at = [self.cursor.x as f32, self.cursor.y as f32];
                self.menu.open_at(at);
                *self.menu_open_at.borrow_mut() = self.menu.is_open().then_some(at);
                *ctx.handled = true;
            }
            _ => {}
        }
        self.events.borrow_mut().push(event.clone());
    }

    fn claims_raw_touch(&self, _point: Point) -> bool {
        self.raw_touch
    }

    fn name(&self) -> &'static str {
        "Probe"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

struct Harness {
    driver: Driver,
    events: Rc<RefCell<Vec<WindowEvent>>>,
    menu_open_at: Rc<RefCell<Option<[f32; 2]>>>,
}

fn harness(raw_touch: bool) -> Harness {
    let events = Rc::new(RefCell::new(Vec::new()));
    let menu_open_at = Rc::new(RefCell::new(None));
    let mut menu = ContextMenu::new();
    menu.push(MenuItem::action("Inspect", || {}));
    let probe = Probe {
        rect: Rect::ZERO,
        raw_touch,
        cursor: Point::new(-1.0, -1.0),
        menu,
        events: events.clone(),
        menu_open_at: menu_open_at.clone(),
    };
    let driver = Driver::new(State::new(), Box::new(probe), (600, 400));
    events.borrow_mut().clear();
    Harness {
        driver,
        events,
        menu_open_at,
    }
}

#[test]
fn spreading_two_fingers_delivers_a_pinch_anchored_at_their_centre() {
    let mut h = harness(false);
    h.driver.touch(1, TouchPhase::Started, 100.0, 100.0);
    h.driver.touch(2, TouchPhase::Started, 200.0, 100.0);
    for x in [220.0, 240.0, 260.0] {
        h.driver.touch(2, TouchPhase::Moved, x, 100.0);
    }
    h.driver.touch(2, TouchPhase::Ended, 260.0, 100.0);
    h.driver.touch(1, TouchPhase::Ended, 100.0, 100.0);

    let events = h.events.borrow();
    let pinches: Vec<(f64, TouchPhase)> = events
        .iter()
        .filter_map(|event| match event {
            WindowEvent::PinchGesture { delta, phase, .. } => Some((*delta, *phase)),
            _ => None,
        })
        .collect();
    let scale: f64 = pinches.iter().map(|(delta, _)| 1.0 + delta).product();
    assert!(
        (scale - 1.6).abs() < 1e-6,
        "the pinch deltas compose to the fingers' spread: {pinches:?}"
    );
    assert_eq!(pinches.first().map(|p| p.1), Some(TouchPhase::Started));
    assert_eq!(pinches.last().map(|p| p.1), Some(TouchPhase::Ended));

    let first_pinch = events
        .iter()
        .position(|event| matches!(event, WindowEvent::PinchGesture { .. }))
        .unwrap();
    let anchor = events[..first_pinch]
        .iter()
        .rev()
        .find_map(|event| match event {
            WindowEvent::CursorMoved { position, .. } => Some((position.x, position.y)),
            _ => None,
        })
        .expect("the pointer moves to the pinch before it zooms");
    assert_eq!(anchor, (160.0, 100.0));
    assert!(
        !events.iter().any(|event| matches!(
            event,
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. }
        )),
        "a pinch neither clicks nor scrolls"
    );
}

#[test]
fn two_fingers_dragging_together_scroll() {
    let mut h = harness(false);
    h.driver.touch(1, TouchPhase::Started, 100.0, 300.0);
    h.driver.touch(2, TouchPhase::Started, 200.0, 300.0);
    for y in [280.0, 260.0, 240.0] {
        h.driver.touch(1, TouchPhase::Moved, 100.0, y);
        h.driver.touch(2, TouchPhase::Moved, 200.0, y);
    }
    h.driver.touch(1, TouchPhase::Ended, 100.0, 240.0);
    h.driver.touch(2, TouchPhase::Ended, 200.0, 240.0);

    let events = h.events.borrow();
    let scrolled: f64 = events
        .iter()
        .filter_map(|event| match event {
            WindowEvent::MouseWheel {
                delta: winit::event::MouseScrollDelta::PixelDelta(delta),
                ..
            } => Some(delta.y),
            _ => None,
        })
        .sum();
    assert!((scrolled + 60.0).abs() < 1e-6, "scrolled {scrolled}");
    assert!(events.iter().any(|event| matches!(
        event,
        WindowEvent::MouseWheel {
            phase: TouchPhase::Ended,
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, WindowEvent::PinchGesture { .. })),
        "a parallel drag is not a pinch"
    );
}

#[test]
fn a_still_hold_opens_the_context_menu_where_the_finger_rests() {
    let mut h = harness(false);
    h.driver.touch_hold_at(120.0, 90.0);
    assert_eq!(*h.menu_open_at.borrow(), Some([120.0, 90.0]));
    assert!(
        !h.events.borrow().iter().any(|event| matches!(
            event,
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                ..
            }
        )),
        "lifting after the hold does not also click"
    );
}

#[test]
fn overlapping_fingers_on_a_raw_touch_widget_are_each_delivered() {
    let mut h = harness(true);
    h.driver.touch(1, TouchPhase::Started, 50.0, 300.0);
    h.driver.touch(2, TouchPhase::Started, 500.0, 300.0);
    h.driver.touch(1, TouchPhase::Ended, 50.0, 300.0);
    h.driver.touch(2, TouchPhase::Ended, 500.0, 300.0);

    let events = h.events.borrow();
    let touches: Vec<(u64, TouchPhase)> = events
        .iter()
        .filter_map(|event| match event {
            WindowEvent::Touch(touch) => Some((touch.id, touch.phase)),
            _ => None,
        })
        .collect();
    assert_eq!(
        touches,
        vec![
            (1, TouchPhase::Started),
            (2, TouchPhase::Started),
            (1, TouchPhase::Ended),
            (2, TouchPhase::Ended),
        ]
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::PinchGesture { .. }
        )),
        "raw-touch contacts synthesise no pointer events: {events:?}"
    );
}
