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
    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _text, _theme, mut env) = ctx.parts();
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

/// A widget that paints what its update took up, and keeps no flag of its own.
struct Gauge {
    value: Rc<Cell<f32>>,
    shown: f32,
    rect: Rect,
    /// How often it drew rather than having its column replay its drawing.
    draws: Rc<Cell<u32>>,
}

impl Gauge {
    fn new(value: &Rc<Cell<f32>>, draws: &Rc<Cell<u32>>) -> Self {
        Self {
            value: Rc::clone(value),
            shown: value.get(),
            rect: Rect::ZERO,
            draws: Rc::clone(draws),
        }
    }
}

#[test]
fn a_sync_redraws_the_columns_above_what_it_moved_and_no_other() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((320, 120), Theme::dark());
    let (clock, clock_draws) = (Rc::new(Cell::new(40.0f32)), Rc::new(Cell::new(0)));
    let (note, note_draws) = (Rc::new(Cell::new(80.0f32)), Rc::new(Cell::new(0)));
    let column = |value, draws| Box::new(Column::new().push(Box::new(Gauge::new(value, draws))));
    let mut screen = Column::new()
        .push(column(&clock, &clock_draws))
        .push(column(&note, &note_draws));
    let first = frame_at(&mut frame, &mut screen, t0);
    crate::widget::Widget::mark_drawn(&mut screen);
    let draws = || (clock_draws.get(), note_draws.get());
    assert_eq!(draws(), (1, 1), "the first frame draws both");

    clock.set(140.0);
    let unreported = frame_at(&mut frame, &mut screen, t0 + Duration::from_millis(16));
    assert!(
        first.encoding_eq(&unreported) && draws() == (1, 1),
        "a widget that keeps no flag of its own reports nothing by itself"
    );
    assert!(
        screen.update(&frame.state.doc, &frame.state.root),
        "the sync's update took up the new value"
    );
    let synced = frame_at(&mut frame, &mut screen, t0 + Duration::from_millis(32));
    assert!(
        !first.encoding_eq(&synced),
        "the screen shows the new value"
    );
    assert_eq!(
        draws(),
        (2, 1),
        "the clock's columns draw it again; the note's column replays what it drew"
    );
    crate::widget::Widget::mark_drawn(&mut screen);
    assert!(
        !screen.update(&frame.state.doc, &frame.state.root),
        "nothing else moved"
    );
    let rested = frame_at(&mut frame, &mut screen, t0 + Duration::from_millis(48));
    assert_eq!(draws(), (2, 1), "at rest nothing draws");
    assert!(
        synced.encoding_eq(&rested),
        "and the screen is the one the sync drew"
    );
}

/// A tree four columns deep: a section holding a gauge beside two columns, and a column three
/// deep holding the last. Each gauge comes back with its value and how often it drew.
fn deep_screen() -> (Column, [(Rc<Cell<f32>>, Rc<Cell<u32>>); 4]) {
    let gauges = [40.0f32, 60.0, 80.0, 100.0]
        .map(|value| (Rc::new(Cell::new(value)), Rc::new(Cell::new(0))));
    let gauge = |at: usize| Box::new(Gauge::new(&gauges[at].0, &gauges[at].1));
    let column = |child: Box<dyn Widget>| Box::new(Column::new().push(child));
    let section = Column::new()
        .push(gauge(0))
        .push(column(gauge(1)))
        .push(column(gauge(2)));
    let screen = Column::new()
        .push(Box::new(section))
        .push(column(column(column(gauge(3)))));
    (screen, gauges)
}

/// The paths every column in `widget`'s tree keeps to replay.
fn kept_paths(widget: &dyn Widget) -> u32 {
    let own = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<Column>())
        .map_or(0, Column::kept_paths);
    own + widget.children().into_iter().map(kept_paths).sum::<u32>()
}

#[test]
fn a_sync_deep_in_a_tree_redraws_only_what_moved() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((320, 240), Theme::dark());
    let (mut screen, gauges) = deep_screen();
    let draws = || gauges.each_ref().map(|(_, draws)| draws.get());
    let mut at = t0;
    let mut next_frame = |frame: &mut ShotFrame, screen: &mut Column| {
        at += Duration::from_millis(16);
        let scene = frame_at(frame, screen, at);
        crate::widget::Widget::mark_drawn(screen);
        scene
    };
    next_frame(&mut frame, &mut screen);
    assert_eq!(draws(), [1, 1, 1, 1], "the first frame draws every gauge");

    for (moved, expected) in [(1, [1, 2, 1, 1]), (0, [2, 2, 1, 1]), (3, [2, 2, 1, 2])] {
        gauges[moved].0.set(gauges[moved].0.get() + 50.0);
        assert!(
            screen.update(&frame.state.doc, &frame.state.root),
            "the sync's update took up gauge {moved}'s value"
        );
        let synced = next_frame(&mut frame, &mut screen);
        assert_eq!(
            draws(),
            expected,
            "gauge {moved} draws again, and every other gauge replays what it drew"
        );
        let rested = next_frame(&mut frame, &mut screen);
        assert_eq!(draws(), expected, "at rest nothing draws");
        assert!(
            synced.encoding_eq(&rested),
            "and the screen is the one the sync drew"
        );
    }
}

#[test]
fn a_replayed_tree_keeps_each_drawing_once() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((320, 240), Theme::dark());
    let (mut screen, gauges) = deep_screen();
    let drawn = frame_at(&mut frame, &mut screen, t0);
    crate::widget::Widget::mark_drawn(&mut screen);
    let replayed = frame_at(&mut frame, &mut screen, t0 + Duration::from_millis(16));
    assert!(
        drawn.encoding_eq(&replayed),
        "the second frame replays the first"
    );
    assert_eq!(
        (replayed.encoding().n_paths, kept_paths(&screen)),
        (4, 4),
        "the frame shows four gauges, and the tree keeps each one's drawing once, \
         not once more for every column above it"
    );

    gauges[1].0.set(140.0);
    assert!(screen.update(&frame.state.doc, &frame.state.root));
    frame_at(&mut frame, &mut screen, t0 + Duration::from_millis(32));
    assert_eq!(kept_paths(&screen), 4, "a sync keeps it at one copy");
}

impl Widget for Gauge {
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        let moved = self.shown != self.value.get();
        self.shown = self.value.get();
        moved
    }
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
    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, _text, _theme, mut env) = ctx.parts();
        self.draws.set(self.draws.get() + 1);
        let width = f64::from(self.shown);
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
