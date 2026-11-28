//! A presented [`Fullscreen`] carries its own way out: the control is a real button over the
//! view's leading top corner, announced and reachable, and pressing it reports the view's `exit`
//! event. At rest, the view is the view it holds and nothing else.

use std::collections::HashMap;

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::kurbo::Point;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{component::Described, component::Event, component::Fired, component::Intent};
use tilo_ui_base::{widget::Spacer, widget::Widget};

/// A `Fullscreen` laid out in its own tree, as a window lays its root out, which every event is
/// then dispatched against.
struct Laid {
    widget: Box<dyn Widget>,
    taffy: TaffyTree<()>,
    node: NodeId,
    exits: tilo_ui_base::Intents<Fired>,
}

impl Laid {
    fn of(showing: bool) -> Self {
        let exits = tilo_ui_base::Intents::default();
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
        Some(tilo_ui_base::i18n::widget_text("widget-exit-fullscreen").as_str())
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

/// A view that gives the audience a view of its own asks for the audience's window while it is
/// presented. Until that window is up the view's own window fills the audience's screen; once it
/// is, the view's window stays the presenter's, and the audience's view is laid out on the
/// audience's window and drawn there. The person closing that window ends the presentation, as
/// the way out does.
#[test]
fn a_view_for_the_audience_goes_to_a_window_of_its_own() {
    use tilo_ui_base::fullscreen::{AudiencePresentationRequest, FullscreenTarget};
    use tilo_ui_base::widget::{AudiencePresentationProvider, FullscreenProvider};

    let exits = tilo_ui_base::Intents::default();
    let props = |showing: bool, audience: bool| FullscreenProps {
        key: "page".to_owned(),
        showing,
        exit: Event::View(Intent::new(&exits, "page", "exit")),
        body: Some(Box::new(Spacer::new())),
        audience: audience.then(|| Box::new(Spacer::new()) as Element),
        ..FullscreenProps::default()
    };
    let mut view = Fullscreen::from_props("page", props(false, true));
    assert_eq!(
        view.audience_presentation_request(),
        None,
        "at rest the room is shown nothing"
    );
    assert!(
        view.set_props(props(true, true)),
        "presenting changes it in place"
    );
    assert_eq!(
        view.audience_presentation_request(),
        Some(AudiencePresentationRequest::new(FullscreenTarget::Largest)),
        "presented, it asks for the audience's screen"
    );
    assert_eq!(
        view.fullscreen_intent(),
        Some(true),
        "until the audience's window is up, the view's own fills the screen"
    );
    view.on_audience_presentation_changed(true);
    assert_eq!(
        view.fullscreen_intent(),
        Some(false),
        "with the audience's window up, the view's own stays the presenter's"
    );

    let theme = Theme::dark();
    let window = vello::kurbo::Rect::new(0.0, 0.0, 1920.0, 1080.0);
    let mut scene = vello::Scene::new();
    let drawn = tilo_ui_base::text_render::with_layout_renderer(&theme, |text| {
        view.draw_audience_presentation(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            text,
            &theme,
            window,
        )
    });
    assert!(drawn, "the audience's window shows the audience's view");
    let laid = view
        .audience
        .as_ref()
        .expect("the audience's view")
        .get_rect();
    assert!(
        laid.height() > 0.0 && window.contains_rect(laid),
        "laid out on the audience's window: {laid:?}"
    );

    view.on_audience_presentation_closed();
    assert_eq!(
        exits.take(),
        [Fired {
            key: "page".to_owned(),
            event: "exit".to_owned(),
            value: None,
            gesture: None,
        }],
        "closing the audience's window ends the presentation"
    );
    assert_eq!(view.fullscreen_intent(), Some(true));

    let plain = Fullscreen::from_props("page", props(true, false));
    assert_eq!(
        plain.audience_presentation_request(),
        None,
        "a view without one fills the audience's screen with its own window"
    );
}
