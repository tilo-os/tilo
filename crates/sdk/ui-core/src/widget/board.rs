//! `Board`: cards in lanes, moved between them by dragging (a kanban board,
//! a pipeline, a hand of tasks by status).
//!
//! Each lane is a column with a header and its cards, side by side. The
//! cards are any widgets (usually a pressable [`Card`](super::Card)), so a
//! card opens, takes focus and is read out as itself. Dragging one shows a
//! ghost under the pointer and the insertion line where it would land, over
//! the lane it would land in; releasing it reports one [`BoardDrop`]: the
//! card, the lane and the card it goes before. The board changes nothing
//! itself: the owner turns the drop into one action on its model, and the
//! board shows the card where the model then puts it.
//!
//! A press that never travels past [`DRAG_START_THRESHOLD_PX`] is the card's
//! own click. Once a drag starts, the card under it is told the pointer left,
//! so releasing ends the drag without also opening the card.
//!
//! A card is moved without a pointer through what it is an object of (a
//! status in its page, the action the drop stands for), never only by
//! dragging.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat as _;
use crate::dnd::DRAG_START_THRESHOLD_PX;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Rhythm;
use crate::widget::{CardWidth, Column, Container, Element, EventCtx, Row, Widget};

/// Where a dragged card was dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardDrop {
    /// The card's key.
    pub card: String,
    pub lane: String,
    /// The card it goes before; `None` for the end of the lane.
    pub before: Option<String>,
}

/// One lane's keys, in the order its widgets sit.
struct LaneKeys {
    key: String,
    cards: Vec<String>,
}

/// A drag from press to release.
struct Drag {
    card: String,
    /// The pressed card's rect and where in it the pointer took it.
    rect: Rect,
    grab: Point,
    start: Point,
    active: bool,
    /// The lane and the card it would go before, while over a lane.
    target: Option<(usize, Option<String>)>,
}

/// See the [module docs](self).
pub struct Board {
    row: Row,
    lanes: Vec<LaneKeys>,
    drag: Option<Drag>,
    cursor: Point,
    dirty: bool,
    on_drop: Option<Box<dyn FnMut(BoardDrop)>>,
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

impl Board {
    #[must_use]
    pub fn new() -> Self {
        Self {
            row: Row::new().no_wrap().align_start(),
            lanes: Vec::new(),
            drag: None,
            cursor: Point::ZERO,
            dirty: false,
            on_drop: None,
        }
    }

    /// Add a lane keyed `key`, headed by `header`, holding `cards` (each
    /// with its key) in order.
    #[must_use]
    pub fn lane(
        mut self,
        key: impl Into<String>,
        header: Element,
        cards: Vec<(String, Element)>,
    ) -> Self {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Related)
            .push(header);
        let mut keys = Vec::with_capacity(cards.len());
        for (card, widget) in cards {
            keys.push(card);
            column = column.push(widget);
        }
        // The lane's width is a card's comfortable measure, set at layout
        // from the theme (`Board::size_lanes`).
        let lane = Container::new()
            .with_no_padding()
            .with_width(0.0)
            .with_child(Box::new(column));
        self.row = std::mem::take(&mut self.row).push(Box::new(lane));
        self.lanes.push(LaneKeys {
            key: key.into(),
            cards: keys,
        });
        self
    }

    /// Called with each drop.
    #[must_use]
    pub fn on_drop(mut self, on_drop: impl FnMut(BoardDrop) + 'static) -> Self {
        self.on_drop = Some(Box::new(on_drop));
        self
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.row = std::mem::take(&mut self.row).with_id(id);
        self
    }

    /// Whether a drag is going on: the ghost and insertion line show.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.active)
    }

    /// Each lane's rect, in lane order.
    fn lane_rects(&self) -> Vec<Rect> {
        self.row
            .children()
            .iter()
            .map(|lane| lane.get_rect())
            .collect()
    }

    /// Each card's rect in lane `lane`, in order.
    fn card_rects(&self, lane: usize) -> Vec<Rect> {
        let children = self.row.children();
        let Some(container) = children.get(lane) else {
            return Vec::new();
        };
        let inner = container.children();
        let Some(column) = inner.first() else {
            return Vec::new();
        };
        // The first child is the lane's header.
        column
            .children()
            .iter()
            .skip(1)
            .map(|card| card.get_rect())
            .collect()
    }

    /// The card under `point`: its lane, key and rect.
    fn card_at(&self, point: Point) -> Option<(String, Rect)> {
        self.lanes.iter().enumerate().find_map(|(lane, keys)| {
            self.card_rects(lane)
                .into_iter()
                .zip(&keys.cards)
                .find(|(rect, _)| rect.contains(point))
                .map(|(rect, key)| (key.clone(), rect))
        })
    }

    /// Where a card dragged to `point` lands: the lane whose band holds the
    /// pointer, before the first card whose middle is below it.
    fn target_at(&self, point: Point, dragged: &str) -> Option<(usize, Option<String>)> {
        let lane = self
            .lane_rects()
            .iter()
            .position(|rect| point.x >= rect.x0 && point.x < rect.x1)?;
        let keys = self.lanes.get(lane)?;
        let before = self
            .card_rects(lane)
            .into_iter()
            .zip(&keys.cards)
            .filter(|(_, key)| key.as_str() != dragged)
            .find(|(rect, _)| point.y < rect.center().y)
            .map(|(_, key)| key.clone());
        Some((lane, before))
    }

    /// Give every lane a regular card's measure under `theme`, so lanes
    /// follow the text size the cards in them are set in.
    fn size_lanes(&mut self, theme: &Theme) {
        #[allow(clippy::cast_possible_truncation, reason = "a lane width fits f32")]
        let width = CardWidth::Regular.min_width(theme) as f32;
        for lane in self.row.children_mut() {
            if let Some(lane) = lane
                .as_any_mut()
                .and_then(|lane| lane.downcast_mut::<Container>())
            {
                lane.set_width(width);
            }
        }
    }

    /// Where the insertion line goes for `target`, across its lane.
    fn line_rect(&self, (lane, before): &(usize, Option<String>), theme: &Theme) -> Option<Rect> {
        let band = *self.lane_rects().get(*lane)?;
        let keys = self.lanes.get(*lane)?;
        let rects = self.card_rects(*lane);
        let gap = f64::from(theme.tokens.spacing.s2);
        let line = f64::from(theme.tokens.metrics.drag_line_px);
        let y = match before {
            Some(before) => {
                let at = keys.cards.iter().position(|key| key == before)?;
                rects.get(at)?.y0 - gap / 2.0
            }
            None => rects
                .last()
                .map_or(band.y0 + gap, |rect| rect.y1 + gap / 2.0),
        };
        Some(Rect::new(band.x0, y - line / 2.0, band.x1, y + line / 2.0))
    }

    /// The pointer left every card: a drag started, so the press that began
    /// it is not a click.
    fn cancel_child_press(&mut self, event_ctx: &mut EventCtx<'_>) {
        let away = WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(-1.0e6, -1.0e6),
        };
        Widget::handle_event(&mut self.row, &away, event_ctx);
    }

    fn pointer(&self, position: winit::dpi::PhysicalPosition<f64>, scale: f64) -> Point {
        let _ = self;
        Point::new(position.x / scale, position.y / scale)
    }
}

impl crate::widget::Decorator for Board {
    type Inner = Row;

    fn inner(&self) -> &Row {
        &self.row
    }

    fn inner_mut(&mut self) -> &mut Row {
        &mut self.row
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || Widget::needs_redraw(&self.row)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.size_lanes(theme);
        Widget::build_layout(&mut self.row, taffy, theme, node_map)
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = self.pointer(*position, ctx.scale_factor);
                let cursor = self.cursor;
                let mut started = false;
                if let Some(drag) = self.drag.as_mut() {
                    if !drag.active && drag.start.distance(cursor) > DRAG_START_THRESHOLD_PX {
                        drag.active = true;
                        started = true;
                    }
                    if drag.active {
                        let card = drag.card.clone();
                        let target = self.target_at(cursor, &card);
                        if let Some(drag) = self.drag.as_mut() {
                            drag.target = target;
                        }
                        self.dirty = true;
                        if started {
                            self.cancel_child_press(ctx);
                        }
                        *ctx.handled = true;
                        return;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some((card, rect)) = self.card_at(self.cursor) {
                    self.drag = Some(Drag {
                        card,
                        rect,
                        grab: Point::new(self.cursor.x - rect.x0, self.cursor.y - rect.y0),
                        start: self.cursor,
                        active: false,
                        target: None,
                    });
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(drag) = self.drag.take()
                    && drag.active
                {
                    self.dirty = true;
                    if let Some((lane, before)) = drag.target
                        && let Some(keys) = self.lanes.get(lane)
                        && let Some(on_drop) = self.on_drop.as_mut()
                    {
                        on_drop(BoardDrop {
                            card: drag.card,
                            lane: keys.key.clone(),
                            before,
                        });
                    }
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && event.logical_key
                        == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                    && self.is_dragging() =>
            {
                self.drag = None;
                self.dirty = true;
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        Widget::handle_event(&mut self.row, event, ctx);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Widget::draw(
            &mut self.row,
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        self.dirty = false;
        let Some(drag) = self.drag.as_ref().filter(|drag| drag.active) else {
            return;
        };
        let colors = &theme.tokens.colors.intent;
        let radius = f64::from(theme.tokens.radii.control);
        let line_px = f64::from(theme.tokens.metrics.drag_line_px);
        if let Some(target) = &drag.target {
            if let Some(band) = self.lane_rects().get(target.0) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.selection.with_alpha_factor(0.35),
                    None,
                    &RoundedRect::from_rect(*band, radius),
                );
            }
            if let Some(line) = self.line_rect(target, theme) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.accent,
                    None,
                    &RoundedRect::from_rect(line, line_px / 2.0),
                );
            }
        }
        let origin = Point::new(self.cursor.x - drag.grab.x, self.cursor.y - drag.grab.y);
        let ghost = Rect::from_origin_size(origin, drag.rect.size());
        let shape = RoundedRect::from_rect(ghost, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.selection.with_alpha_factor(0.6),
            None,
            &shape,
        );
        scene.stroke(
            &Stroke::new(line_px),
            transform,
            colors.accent,
            None,
            &shape,
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::widget::{Spacer, Text};

    fn card() -> Element {
        Box::new(
            Container::new()
                .with_size(200.0, 60.0)
                .with_child(Box::new(Spacer::fixed(1.0))),
        )
    }

    fn board(drops: &Rc<RefCell<Vec<BoardDrop>>>) -> Board {
        let sink = Rc::clone(drops);
        Board::new()
            .lane(
                "todo",
                Box::new(Text::new("Todo")),
                vec![("a".into(), card()), ("b".into(), card())],
            )
            .lane(
                "done",
                Box::new(Text::new("Done")),
                vec![("c".into(), card())],
            )
            .on_drop(move |drop| sink.borrow_mut().push(drop))
    }

    fn harness(board: Board) -> crate::headless::FrameHarness {
        crate::headless::FrameHarness::for_root(
            crate::State::new(),
            board,
            (900, 600),
            Theme::dark(),
        )
    }

    #[test]
    fn dragging_a_card_into_another_lane_reports_one_drop_before_the_card_under_it() {
        let drops = Rc::new(RefCell::new(Vec::new()));
        let mut host = harness(board(&drops));
        let root = host.root::<Board>();
        let a = root.card_rects(0)[0];
        let c = root.card_rects(1)[0];
        host.drag_at((a.center().x, a.center().y), (c.center().x, c.y0 + 5.0));
        assert_eq!(
            drops.borrow().as_slice(),
            [BoardDrop {
                card: "a".into(),
                lane: "done".into(),
                before: Some("c".into()),
            }]
        );
        assert!(!host.root::<Board>().is_dragging());
    }

    #[test]
    fn dropping_below_every_card_lands_at_the_end_and_a_click_is_no_drop() {
        let drops = Rc::new(RefCell::new(Vec::new()));
        let mut host = harness(board(&drops));
        let root = host.root::<Board>();
        let a = root.card_rects(0)[0];
        let b = root.card_rects(0)[1];
        host.click_at(a.center().x, a.center().y);
        assert!(drops.borrow().is_empty(), "a click moves nothing");
        host.drag_at((a.center().x, a.center().y), (b.center().x, b.y1 + 40.0));
        assert_eq!(
            drops.borrow().as_slice(),
            [BoardDrop {
                card: "a".into(),
                lane: "todo".into(),
                before: None,
            }]
        );
    }
}
