//! A pen's pressure, tilt and eraser reach the widget handling the pointer
//! event they came with, through the same dispatch as every other input.

use super::*;
use crate::widget::PenSample;
use std::{cell::RefCell, rc::Rc};
use winit::event::DeviceId;

/// Records what each pointer event it handles says about the pen.
struct Probe {
    rect: Rect,
    seen: Rc<RefCell<Vec<(&'static str, Option<PenSample>)>>>,
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
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x,
            origin.y,
            origin.x + f64::from(layout.size.width),
            origin.y + f64::from(layout.size.height),
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let kind = match event {
            WindowEvent::CursorMoved { .. } => "move",
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => "press",
            WindowEvent::MouseInput { .. } => "release",
            WindowEvent::PenSample { .. } => "sample",
            _ => return,
        };
        self.seen.borrow_mut().push((kind, ctx.pen()));
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "Probe"
    }
}

fn sample(pressure: f64, eraser: bool) -> WindowEvent {
    WindowEvent::PenSample {
        device_id: DeviceId::dummy(),
        pressure,
        tilt: (25.0, -40.0),
        eraser,
    }
}

fn moved(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(x, y),
    }
}

fn button(state: ElementState) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state,
        button: winit::event::MouseButton::Left,
    }
}

fn pen(pressure: f64, eraser: bool) -> Option<PenSample> {
    Some(PenSample {
        pressure,
        tilt: (25.0, -40.0),
        eraser,
    })
}

#[test]
fn a_widget_reads_the_pen_behind_each_pointer_event_and_none_for_a_mouse() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut frame = FrameHarness::new_with_controlled_inputs(
        State::new(),
        Box::new(Probe {
            rect: Rect::ZERO,
            seen: seen.clone(),
        }),
        (320, 200),
        Theme::dark(),
        crate::font_catalog::fixture_renderer(),
        crate::permissions::UserId("pen-review".into()),
    );

    // A stroke: hover in, touch down harder and harder, lift.
    for event in [
        sample(0.0, false),
        moved(40.0, 50.0),
        sample(0.3, false),
        button(ElementState::Pressed),
        sample(0.8, false),
        moved(60.0, 70.0),
        sample(0.0, false),
        button(ElementState::Released),
    ] {
        frame.send_host_event(&event);
    }
    // The eraser end, then the mouse again.
    frame.send_host_event(&sample(0.5, true));
    frame.send(&moved(80.0, 90.0));
    frame.send_host_event(&moved(100.0, 110.0));
    frame.send_host_event(&button(ElementState::Pressed));

    assert_eq!(
        *seen.borrow(),
        [
            ("move", pen(0.0, false)),
            ("press", pen(0.3, false)),
            ("move", pen(0.8, false)),
            ("release", pen(0.0, false)),
            ("move", pen(0.5, true)),
            ("move", None),
            ("press", None),
        ],
        "the samples themselves never reach a widget as events"
    );
}
