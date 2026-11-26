//! A presented [`Fullscreen`] carries its own way out: the control is a real button over the
//! view's leading top corner, announced and reachable, and pressing it reports the view's `exit`
//! event. At rest, the view is the view it holds and nothing else.

use std::collections::HashMap;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::*;
use crate::component::{Described, Event, Fired, Intent};
use crate::theme::Theme;
use crate::widget::{Spacer, Widget};

/// A `Fullscreen` laid out in its own tree, as a window lays its root out, which every event is
/// then dispatched against.
struct Laid {
    widget: Box<dyn Widget>,
    taffy: TaffyTree<()>,
    node: NodeId,
    exits: crate::Intents<Fired>,
}

impl Laid {
    fn of(showing: bool) -> Self {
        let exits = crate::Intents::default();
        let mut widget = Fullscreen::from_props(
            "page",
            FullscreenProps {
                key: "page".to_owned(),
                showing,
                exit: Event::View(Intent::new(&exits, "page", "exit")),
                body: Some(Box::new(Spacer::new())),
                ..FullscreenProps::default()
            },
        );
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes = HashMap::new();
        let node = Widget::build_layout(&mut widget, &mut taffy, &Theme::dark(), &mut nodes);
        taffy
            .compute_layout(
                node,
                Size {
                    width: AvailableSpace::Definite(800.0),
                    height: AvailableSpace::Definite(600.0),
                },
            )
            .expect("the view solves its layout");
        Widget::apply_layout(&mut widget, &taffy, node, Point::ZERO);
        Self {
            widget: Box::new(widget),
            taffy,
            node,
            exits,
        }
    }

    /// The way out the presented view carries: the child after the view it presents.
    fn way_out(&self) -> Option<&dyn Widget> {
        let children = self.widget.children();
        (children.len() > 1).then(|| children[1])
    }

    fn children(&self) -> usize {
        self.widget.children().len()
    }

    fn move_to(&mut self, at: Point) {
        self.send(&WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(at.x, at.y),
        });
    }

    fn press_at(&mut self, at: Point) {
        self.move_to(at);
        for state in [ElementState::Pressed, ElementState::Released] {
            self.send(&WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state,
                button: MouseButton::Left,
            });
        }
    }

    fn send(&mut self, event: &WindowEvent) {
        let mut doc = automerge::AutoCommit::new();
        crate::testing::Dispatch::new()
            .doc(&mut doc)
            .layout(&self.taffy, self.node)
            .send(self.widget.as_mut(), event);
    }

    fn fired(&self) -> Vec<Fired> {
        self.exits.take()
    }
}

/// A presented view carries the way out in its leading top corner, and pressing it reports the
/// view's exit once.
#[test]
fn a_presented_view_carries_a_way_out_that_reports_it() {
    let mut laid = Laid::of(true);
    assert_eq!(laid.children(), 2, "the view and its way out");
    let way_out = laid
        .way_out()
        .expect("a presented view carries the way out");
    let rect = way_out.get_rect();
    assert!(
        rect.width() > 0.0 && rect.height() > 0.0,
        "the way out is drawn: {rect:?}"
    );
    assert!(
        rect.x0 >= 0.0 && rect.y0 >= 0.0 && rect.x1 <= 800.0 && rect.y1 <= 600.0,
        "it stands inside the view: {rect:?}"
    );
    assert!(
        rect.x0 < 800.0 / 2.0 && rect.y0 < 600.0 / 2.0,
        "at the leading top corner: {rect:?}"
    );
    // It is announced as what it does, not as a glyph.
    let announced = way_out.accessibility().and_then(|node| node.name);
    assert_eq!(
        announced.as_deref(),
        Some(crate::i18n::widget_text("widget-exit-fullscreen").as_str())
    );

    laid.send(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: PhysicalPosition::new(rect.center().x, rect.center().y),
    });
    laid.press_at(rect.center());
    assert_eq!(
        laid.fired(),
        [Fired {
            key: "page".to_owned(),
            event: "exit".to_owned(),
            value: None,
            gesture: None,
        }]
    );
}

/// At rest the view is the view it holds: nothing of the way out is drawn, reached or pressed.
#[test]
fn a_view_at_rest_is_only_the_view_it_holds() {
    let mut laid = Laid::of(false);
    assert_eq!(laid.children(), 1, "the view it holds, and nothing else");
    laid.press_at(Point::new(10.0, 10.0));
    assert!(laid.fired().is_empty(), "nothing of the view was pressed");
}
