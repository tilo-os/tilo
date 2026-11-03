use super::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::launch::ShotFrame;
use crate::time::{Duration, testing::with_monotonic_time};
use crate::widget::{Chip, Column, Tooltip};
use taffy::{Size, Style};
use vello::kurbo::Rect;

/// One frame of `widget` at `at`: sync, layout, draw.
fn frame_at(frame: &mut ShotFrame, widget: &mut dyn Widget, at: crate::time::Instant) -> Scene {
    with_monotonic_time(at, || {
        frame.sync(widget);
        frame.layout(widget).expect("layout solves");
        frame.draw(widget)
    })
}

#[test]
fn a_tooltip_fading_in_inside_a_flow_paints_new_pixels_each_frame() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((320, 200), Theme::dark());
    let tip = with_monotonic_time(t0, || {
        let mut tip = Tooltip::new("Saved")
            .with_delay(Duration::ZERO)
            .with_anchor(Rect::new(16.0, 16.0, 96.0, 48.0));
        tip.on_hover_enter(Duration::ZERO);
        tip
    });
    let mut column = Column::new().push(Box::new(tip));

    let first = frame_at(&mut frame, &mut column, t0);
    let moving = with_monotonic_time(t0, || column.needs_animation_frame());
    assert!(moving, "the fade started when the frame drew it");

    let later = t0 + Duration::from_millis(90);
    let second = frame_at(&mut frame, &mut column, later);

    assert!(
        !first.encoding_eq(&second),
        "the flow's scene is only as fresh as the child that moves: a fade \
         must repaint through the cache"
    );
}

#[test]
fn a_leaf_changed_by_a_setter_repaints() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((320, 120), Theme::dark());
    let mut chip = Chip::new("alpha").selectable().selected(false);

    let first = frame_at(&mut frame, &mut chip, t0);
    crate::widget::Widget::mark_drawn(&mut chip);
    assert!(
        !with_monotonic_time(t0, || chip.needs_redraw()),
        "the frame that drew it rested it"
    );

    chip.set_selected(true);
    assert!(
        with_monotonic_time(t0, || chip.needs_redraw()),
        "a setter that changes the paint asks for a frame"
    );

    let second = frame_at(&mut frame, &mut chip, t0 + Duration::from_millis(16));
    crate::widget::Widget::mark_drawn(&mut chip);
    assert!(
        !first.encoding_eq(&second),
        "the selection paints new pixels"
    );
    assert!(
        !with_monotonic_time(t0 + Duration::from_millis(16), || chip.needs_redraw()),
        "and it rests after that frame"
    );
}

/// Paints a width it reads from outside itself, and keeps no flag of its
/// own: what it draws is whatever it was handed last.
struct Readout {
    width: Rc<Cell<f32>>,
    rect: Rect,
}

impl Widget for Readout {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_lengths(200.0, 40.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }
    crate::rect_geometry!();
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text: &mut crate::text_render::TextRenderer,
        _theme: &Theme,
        _presence: &crate::presence::PresenceCtx,
    ) {
        // The width stands outside this widget: the drawing depends on
        // readings no flag of its own reports.
        crate::outside_paint::note();
        let width = f64::from(self.width.get());
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            vello::peniko::Color::BLACK,
            None,
            &Rect::new(0.0, 0.0, width, 40.0),
        );
    }
    fn name(&self) -> &'static str {
        "Readout"
    }
}

#[test]
fn a_cache_that_read_outside_state_repaints_once_input_arrives() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((320, 120), Theme::dark());
    let width = Rc::new(Cell::new(40.0f32));
    let mut column = Column::new().push(Box::new(Readout {
        width: Rc::clone(&width),
        rect: Rect::ZERO,
    }));

    let first = frame_at(&mut frame, &mut column, t0);
    crate::widget::Widget::mark_drawn(&mut column);
    let at_rest = frame_at(&mut frame, &mut column, t0 + Duration::from_millis(16));
    assert!(
        first.encoding_eq(&at_rest),
        "a frame that nothing reached replays the drawing the cache kept"
    );

    width.set(140.0);
    let unreported = frame_at(&mut frame, &mut column, t0 + Duration::from_millis(32));
    assert!(
        at_rest.encoding_eq(&unreported),
        "what no input carried and no flag reported is what the cache holds"
    );

    crate::input_generation::note();
    let handed = frame_at(&mut frame, &mut column, t0 + Duration::from_millis(48));
    assert!(
        !at_rest.encoding_eq(&handed),
        "input reached the tree: a cache whose drawing read state outside it \
         paints that again, not the drawing kept from before"
    );
}

#[test]
fn a_cache_that_read_nothing_outside_keeps_its_drawing_across_input() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((320, 120), Theme::dark());
    let mut column = Column::new().push(Box::new(Chip::new("alpha")));

    let first = frame_at(&mut frame, &mut column, t0);
    crate::widget::Widget::mark_drawn(&mut column);
    crate::input_generation::note();
    let after_input = frame_at(&mut frame, &mut column, t0 + Duration::from_millis(16));
    assert!(
        first.encoding_eq(&after_input),
        "input changes nothing a cache that read inside the tree paints: it \
         keeps its drawing, which is what lets a scroll frame copy"
    );
}
