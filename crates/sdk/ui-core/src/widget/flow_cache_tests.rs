use super::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::launch::ShotFrame;
use crate::time::{Duration, testing::with_monotonic_time};
use crate::widget::{Chip, Column, Tooltip};
use taffy::{Size, Style};
use vello::kurbo::Rect;

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

struct Gauge {
    value: Rc<Cell<f32>>,
    rect: Rect,
}

impl Widget for Gauge {
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
        let width = f64::from(self.value.get());
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            vello::peniko::Color::BLACK,
            None,
            &Rect::new(0.0, 0.0, width, 40.0),
        );
    }
    fn name(&self) -> &'static str {
        "Gauge"
    }
}

#[test]
fn a_cache_repaints_what_a_sync_handed_the_tree() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((320, 120), Theme::dark());
    let value = Rc::new(Cell::new(40.0f32));
    let mut column = Column::new().push(Box::new(Gauge {
        value: Rc::clone(&value),
        rect: Rect::ZERO,
    }));

    let first = frame_at(&mut frame, &mut column, t0);
    crate::widget::Widget::mark_drawn(&mut column);
    let at_rest = frame_at(&mut frame, &mut column, t0 + Duration::from_millis(16));
    assert!(
        first.encoding_eq(&at_rest),
        "a frame that nothing reached replays the drawing the cache kept"
    );

    value.set(140.0);
    let unreported = frame_at(&mut frame, &mut column, t0 + Duration::from_millis(32));
    assert!(
        at_rest.encoding_eq(&unreported),
        "a widget that keeps no flag of its own reports nothing by itself"
    );

    crate::input_generation::note_model();
    let synced = frame_at(&mut frame, &mut column, t0 + Duration::from_millis(48));
    assert!(
        !at_rest.encoding_eq(&synced),
        "the sync handed the tree a changed document: the cache holds a \
         drawing of the one before it, so it draws again"
    );
    crate::widget::Widget::mark_drawn(&mut column);
    let rested = frame_at(&mut frame, &mut column, t0 + Duration::from_millis(64));
    assert!(synced.encoding_eq(&rested), "and it rests on that document");
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
