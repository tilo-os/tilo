//! Week/day timeline interaction controller.
//!
//! This owns the timeline viewport, pointer/scroll capture, and the complete
//! create/move gesture state machine. It resolves screen coordinates into typed
//! effects; [`CalendarApp`](crate::app::CalendarApp) remains responsible for
//! applying those effects to the calendar model and CRDT.

use automerge::{AutoCommit, ObjId};
use chrono::{Datelike, NaiveDate};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

use ui::dnd::DRAG_START_THRESHOLD_PX;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::date;
use crate::draw_util::{elide_to_width, fill_rect, fill_round, stroke_line, stroke_round};
use crate::model::{Id, Palette};
use crate::timeline_viewport::{TimelineGeom, TimelineViewport};

pub(super) const TIMELINE_SNAP_MIN: i64 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TimelineSpan {
    pub id: Id,
    pub day: NaiveDate,
    pub start_min: i64,
    pub end_min: i64,
}

pub(super) struct TimelineRenderEvent<'a> {
    pub id: Id,
    pub day: NaiveDate,
    pub title: &'a str,
    pub location: &'a str,
    pub color: Palette,
    pub start_min: i64,
    pub end_min: i64,
    pub all_day: bool,
}

/// Compact, presentation-ready snapshot supplied by `CalendarApp`. Filtering
/// and display-title policy stay in the host; all timeline paint geometry and
/// hit surfaces stay here.
pub(super) struct TimelineRenderModel<'a, 'event> {
    pub days: &'a [NaiveDate],
    pub events: &'a [TimelineRenderEvent<'event>],
    pub today: NaiveDate,
    pub now: Option<(NaiveDate, i64)>,
    pub untitled: &'a str,
}

/// A domain-neutral result from the pointer controller. Calendar policy and
/// persistence deliberately live in the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TimelineEffect {
    Create {
        day: NaiveDate,
        start_min: i64,
        end_min: i64,
    },
    Move {
        id: Id,
        day: NaiveDate,
        start_min: i64,
        dur_min: i64,
    },
    Select(Id),
    OpenDay(NaiveDate),
}

pub(super) struct TimelineOutcome {
    pub handled: bool,
    pub effect: Option<TimelineEffect>,
}

impl TimelineOutcome {
    fn ignored() -> Self {
        Self {
            handled: false,
            effect: None,
        }
    }

    fn handled(effect: Option<TimelineEffect>) -> Self {
        Self {
            handled: true,
            effect,
        }
    }
}

/// Widget event context needed only because the shared `Scrollable` follows
/// the standard widget event contract. Bundling it keeps that plumbing at the
/// controller boundary rather than threading eight parameters through every
/// gesture branch.
pub(super) struct TimelineEventCtx<'a> {
    pub event: &'a WindowEvent,
    pub scale_factor: f64,
    pub modifiers: Modifiers,
    pub state: &'a mut AutoCommit,
    pub path: &'a ObjId,
    pub focus_target: &'a mut Option<NodeId>,
    pub node_id: NodeId,
    pub taffy: &'a TaffyTree<()>,
}

#[derive(Clone, Copy, Debug)]
enum TimelineDrag {
    Create {
        day: NaiveDate,
        origin: Point,
        current: Point,
        active: bool,
    },
    Move {
        id: Id,
        origin: Point,
        current: Point,
        grab_off_min: i64,
        dur_min: i64,
        active: bool,
    },
}

impl TimelineDrag {
    fn is_active(self) -> bool {
        match self {
            Self::Create { active, .. } | Self::Move { active, .. } => active,
        }
    }

    fn origin(self) -> Point {
        match self {
            Self::Create { origin, .. } | Self::Move { origin, .. } => origin,
        }
    }

    fn set_current(&mut self, point: Point) {
        match self {
            Self::Create { current, .. } | Self::Move { current, .. } => *current = point,
        }
    }

    fn set_active(&mut self) {
        match self {
            Self::Create { active, .. } | Self::Move { active, .. } => *active = true,
        }
    }
}

pub(super) struct TimelineInteraction {
    viewport: TimelineViewport,
    pointer: Point,
    drag: Option<TimelineDrag>,
}

impl TimelineInteraction {
    pub fn new() -> Self {
        Self {
            viewport: TimelineViewport::new(),
            pointer: Point::ZERO,
            drag: None,
        }
    }

    pub fn handle_event(
        &mut self,
        mut cx: TimelineEventCtx<'_>,
        area: Rect,
        days: &[NaiveDate],
        spans: &[TimelineSpan],
    ) -> TimelineOutcome {
        if let WindowEvent::CursorMoved { position, .. } = cx.event {
            self.pointer = Point::new(position.x / cx.scale_factor, position.y / cx.scale_factor);
        }

        match cx.event {
            WindowEvent::MouseWheel { .. } if area.contains(self.pointer) => {
                let handled = self.forward_to_scroll(&mut cx);
                TimelineOutcome {
                    handled,
                    effect: None,
                }
            }
            WindowEvent::CursorMoved { .. } => self.handle_pointer_move(&mut cx, area),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let body = Self::geometry(area).body(area.x0, area.x1);
                if !body.contains(self.pointer) {
                    return TimelineOutcome::ignored();
                }
                if self.forward_to_scroll(&mut cx) {
                    return TimelineOutcome::handled(None);
                }
                if self.begin_drag(area, days, spans, self.pointer) {
                    TimelineOutcome::handled(None)
                } else {
                    TimelineOutcome::ignored()
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.wants_pointer_capture() {
                    let handled = self.forward_to_scroll(&mut cx);
                    return TimelineOutcome {
                        handled,
                        effect: None,
                    };
                }
                if self.drag.is_none() {
                    return TimelineOutcome::ignored();
                }
                TimelineOutcome::handled(self.finish_drag(area, days, spans))
            }
            _ => TimelineOutcome::ignored(),
        }
    }

    fn handle_pointer_move(
        &mut self,
        cx: &mut TimelineEventCtx<'_>,
        area: Rect,
    ) -> TimelineOutcome {
        if self.wants_pointer_capture() {
            let handled = self.forward_to_scroll(cx);
            return TimelineOutcome {
                handled,
                effect: None,
            };
        }
        if self.drag.is_some() {
            self.update_drag(self.pointer);
            return TimelineOutcome::handled(None);
        }
        if area.contains(self.pointer) {
            // Hover tracking for the overlay scrollbar must see the move, but
            // does not own the event when no drag/capture is active.
            self.forward_to_scroll(cx);
        }
        TimelineOutcome::ignored()
    }

    fn begin_drag(
        &mut self,
        area: Rect,
        days: &[NaiveDate],
        spans: &[TimelineSpan],
        point: Point,
    ) -> bool {
        let geom = Self::geometry(area);
        if let Some(span) = self.event_at(area, days, spans, point) {
            let press_min = self.minute_at(geom, point.y);
            self.drag = Some(TimelineDrag::Move {
                id: span.id,
                origin: point,
                current: point,
                grab_off_min: (press_min - span.start_min).clamp(0, span.end_min - span.start_min),
                dur_min: (span.end_min - span.start_min).max(TIMELINE_SNAP_MIN),
                active: false,
            });
            return true;
        }
        if point.x < area.x0 + geom.gutter {
            return false;
        }
        let col = TimelineViewport::column_at(area, days.len(), geom, point.x);
        let Some(&day) = days.get(col) else {
            return false;
        };
        self.drag = Some(TimelineDrag::Create {
            day,
            origin: point,
            current: point,
            active: false,
        });
        true
    }

    fn update_drag(&mut self, point: Point) {
        let Some(drag) = &mut self.drag else {
            return;
        };
        drag.set_current(point);
        if !drag.is_active() && drag.origin().distance(point) >= DRAG_START_THRESHOLD_PX {
            drag.set_active();
        }
    }

    fn event_at(
        &self,
        area: Rect,
        days: &[NaiveDate],
        spans: &[TimelineSpan],
        point: Point,
    ) -> Option<TimelineSpan> {
        let geom = Self::geometry(area);
        if point.x < area.x0 + geom.gutter || point.x > area.x1 {
            return None;
        }
        let col = TimelineViewport::column_at(area, days.len(), geom, point.x);
        let day = *days.get(col)?;
        spans.iter().rev().copied().find(|span| {
            span.day == day
                && self
                    .block_rect(area, days.len(), geom, col, span.start_min, span.end_min)
                    .contains(point)
        })
    }

    fn effect_for_drag(
        &self,
        area: Rect,
        days: &[NaiveDate],
        drag: TimelineDrag,
    ) -> Option<TimelineEffect> {
        let geom = Self::geometry(area);
        match drag {
            TimelineDrag::Create {
                day,
                origin,
                current,
                ..
            } => {
                let a = snap_minute(self.minute_at(geom, origin.y));
                let b = snap_minute(self.minute_at(geom, current.y));
                let (start_min, mut end_min) = (a.min(b), a.max(b));
                if end_min - start_min < TIMELINE_SNAP_MIN {
                    end_min = (start_min + TIMELINE_SNAP_MIN).min(24 * 60);
                }
                Some(TimelineEffect::Create {
                    day,
                    start_min,
                    end_min,
                })
            }
            TimelineDrag::Move {
                id,
                current,
                grab_off_min,
                dur_min,
                ..
            } => {
                let col = TimelineViewport::column_at(area, days.len(), geom, current.x);
                let cursor_min = self.minute_at(geom, current.y);
                let start_min =
                    snap_minute(cursor_min - grab_off_min).clamp(0, (24 * 60 - dur_min).max(0));
                Some(TimelineEffect::Move {
                    id,
                    day: *days.get(col)?,
                    start_min,
                    dur_min,
                })
            }
        }
    }

    fn finish_drag(
        &mut self,
        area: Rect,
        days: &[NaiveDate],
        spans: &[TimelineSpan],
    ) -> Option<TimelineEffect> {
        let drag = self.drag.take()?;
        if drag.is_active() {
            return self.effect_for_drag(area, days, drag);
        }
        let body = Self::geometry(area).body(area.x0, area.x1);
        body.contains(self.pointer)
            .then(|| self.event_at(area, days, spans, self.pointer))
            .flatten()
            .map(|span| TimelineEffect::Select(span.id))
    }

    fn forward_to_scroll(&mut self, cx: &mut TimelineEventCtx<'_>) -> bool {
        self.viewport.handle_event(
            cx.event,
            cx.scale_factor,
            cx.modifiers,
            cx.state,
            cx.path,
            cx.focus_target,
            cx.node_id,
            cx.taffy,
        )
    }

    fn preview(&self, area: Rect, days: &[NaiveDate]) -> Option<TimelineEffect> {
        let drag = self.drag.filter(|drag| drag.is_active())?;
        self.effect_for_drag(area, days, drag)
    }

    fn moving_id(&self) -> Option<Id> {
        match self.drag {
            Some(TimelineDrag::Move {
                id, active: true, ..
            }) => Some(id),
            _ => None,
        }
    }

    pub fn cancel_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }

    /// Paint the complete week/day surface and return its typed hit surfaces.
    /// The app maps these effects to domain actions, just as it does effects
    /// emitted directly by pointer gestures.
    pub fn draw_timeline(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        model: &TimelineRenderModel<'_, '_>,
    ) -> Vec<(Rect, TimelineEffect)> {
        let colors = &theme.tokens.colors;
        let mut hits = Vec::new();
        fill_rect(scene, transform, colors.surfaces.base, area);

        let geom = Self::geometry(area);
        let grid_x0 = area.x0 + geom.gutter;
        let col_w = Self::column_width(area, model.days.len(), geom);
        Self::draw_headers(
            scene, transform, text, theme, area, model, grid_x0, col_w, &mut hits,
        );
        stroke_line(
            scene,
            transform,
            colors.border_standard,
            Point::new(area.x0, geom.body_top),
            Point::new(geom.grid_right, geom.body_top),
            0.5,
        );

        let body = geom.body(area.x0, area.x1);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body,
        );
        self.draw_grid(scene, transform, text, theme, area, model, geom);
        self.draw_events(
            scene, transform, text, theme, area, model, geom, body, &mut hits,
        );
        self.draw_drag_ghost(scene, transform, text, theme, area, model, geom);
        scene.pop_layer();
        self.viewport.draw(scene, transform, theme);
        hits
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_headers(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        model: &TimelineRenderModel<'_, '_>,
        grid_x0: f64,
        col_w: f64,
        hits: &mut Vec<(Rect, TimelineEffect)>,
    ) {
        let colors = &theme.tokens.colors;
        for (index, day) in model.days.iter().enumerate() {
            let x0 = grid_x0 + col_w * index as f64;
            let header = Rect::new(
                x0,
                area.y0,
                x0 + col_w,
                area.y0 + crate::timeline_viewport::TIMELINE_HEAD_H,
            );
            let is_today = *day == model.today;
            let weekend = day.weekday().num_days_from_monday() >= 5;
            if is_today {
                fill_rect(scene, transform, colors.accent[5].with_alpha(0.10), header);
            }
            text.draw_text(
                scene,
                date::WEEKDAY_SHORT
                    .get(day.weekday().num_days_from_monday() as usize)
                    .copied()
                    .unwrap_or(""),
                11.0,
                if weekend {
                    colors.text_dim
                } else {
                    colors.neutral[7]
                },
                transform * Affine::translate((x0 + 10.0, area.y0 + 8.0)),
                None,
            );
            text.draw_text(
                scene,
                &format!("{}", day.day()),
                20.0,
                if is_today {
                    colors.accent[5]
                } else {
                    colors.neutral[9]
                },
                transform * Affine::translate((x0 + 10.0, area.y0 + 24.0)),
                None,
            );
            hits.push((header, TimelineEffect::OpenDay(*day)));

            let strip = Rect::new(
                x0 + 2.0,
                area.y0 + crate::timeline_viewport::TIMELINE_HEAD_H + 2.0,
                x0 + col_w - 2.0,
                area.y0
                    + crate::timeline_viewport::TIMELINE_HEAD_H
                    + crate::timeline_viewport::TIMELINE_ALLDAY_H
                    - 2.0,
            );
            if let Some(event) = model
                .events
                .iter()
                .find(|event| event.day == *day && event.all_day)
            {
                fill_round(scene, transform, event.color.color(theme), strip, 4.0);
                let line = elide_to_width(text, event.title, 10.0, strip.width() - 12.0);
                text.draw_text(
                    scene,
                    &line,
                    10.0,
                    Color::WHITE,
                    transform * Affine::translate((strip.x0 + 6.0, strip.y0 + 4.0)),
                    None,
                );
                hits.push((strip, TimelineEffect::Select(event.id)));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_grid(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        model: &TimelineRenderModel<'_, '_>,
        geom: TimelineGeom,
    ) {
        let colors = &theme.tokens.colors;
        let grid_x0 = area.x0 + geom.gutter;
        let col_w = Self::column_width(area, model.days.len(), geom);
        let offset = f64::from(self.offset_y());
        let y_at = |minute: f64| geom.body_top + minute / 60.0 * geom.hour_h - offset;
        let body = geom.body(area.x0, area.x1);
        for hour in 0..=24 {
            let y = y_at(f64::from(hour) * 60.0);
            stroke_line(
                scene,
                transform,
                colors.border_standard.with_alpha(0.5),
                Point::new(grid_x0, y),
                Point::new(geom.grid_right, y),
                0.5,
            );
            if hour < 24 {
                text.draw_text(
                    scene,
                    &format!("{hour:02}:00"),
                    10.0,
                    colors.text_dim,
                    transform * Affine::translate((area.x0 + 12.0, y + 2.0)),
                    None,
                );
            }
        }
        for index in 0..=model.days.len() {
            let x = grid_x0 + col_w * index as f64;
            stroke_line(
                scene,
                transform,
                colors.border_standard.with_alpha(0.5),
                Point::new(x, body.y0),
                Point::new(x, body.y1),
                0.5,
            );
        }
        if let Some((now_day, now_min)) = model.now
            && let Some(index) = model.days.iter().position(|day| *day == now_day)
        {
            let y = y_at(now_min as f64);
            let x0 = grid_x0 + col_w * index as f64;
            stroke_line(
                scene,
                transform,
                colors.semantic.danger,
                Point::new(x0, y),
                Point::new(x0 + col_w, y),
                2.0,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.semantic.danger,
                None,
                &Circle::new(Point::new(x0, y), 3.5),
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_events(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        model: &TimelineRenderModel<'_, '_>,
        geom: TimelineGeom,
        body: Rect,
        hits: &mut Vec<(Rect, TimelineEffect)>,
    ) {
        let colors = &theme.tokens.colors;
        let moving_id = self.moving_id();
        for (index, day) in model.days.iter().enumerate() {
            for event in model
                .events
                .iter()
                .filter(|event| event.day == *day && !event.all_day && Some(event.id) != moving_id)
            {
                let block = self.block_rect(
                    area,
                    model.days.len(),
                    geom,
                    index,
                    event.start_min,
                    event.end_min,
                );
                let color = event.color.color(theme);
                fill_round(scene, transform, color.with_alpha(0.22), block, 6.0);
                fill_round(
                    scene,
                    transform,
                    color,
                    Rect::new(block.x0, block.y0, block.x0 + 3.0, block.y1),
                    2.0,
                );
                scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &block,
                );
                let text_width = block.width() - 20.0;
                let title = elide_to_width(text, event.title, 11.0, text_width);
                text.draw_text(
                    scene,
                    &title,
                    11.0,
                    colors.neutral[9],
                    transform * Affine::translate((block.x0 + 10.0, block.y0 + 4.0)),
                    None,
                );
                if block.height() > 34.0 {
                    let subtitle = format!(
                        "{}–{} {}",
                        fmt_event_min(event.start_min),
                        fmt_event_min(event.end_min),
                        event.location
                    );
                    let subtitle = elide_to_width(text, &subtitle, 10.0, text_width);
                    text.draw_text(
                        scene,
                        &subtitle,
                        10.0,
                        colors.text_dim,
                        transform * Affine::translate((block.x0 + 10.0, block.y0 + 18.0)),
                        None,
                    );
                }
                scene.pop_layer();
                let visible = block.intersect(body);
                if visible.width() > 0.0 && visible.height() > 0.0 {
                    hits.push((visible, TimelineEffect::Select(event.id)));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_drag_ghost(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        model: &TimelineRenderModel<'_, '_>,
        geom: TimelineGeom,
    ) {
        let colors = &theme.tokens.colors;
        let Some(preview) = self.preview(area, model.days) else {
            return;
        };
        let (column, start_min, end_min, title, fill, stroke) = match preview {
            TimelineEffect::Create {
                day,
                start_min,
                end_min,
            } => {
                let Some(column) = model.days.iter().position(|item| *item == day) else {
                    return;
                };
                (
                    column,
                    start_min,
                    end_min,
                    model.untitled,
                    colors.accent[5].with_alpha(0.28),
                    colors.accent[5],
                )
            }
            TimelineEffect::Move {
                id,
                day,
                start_min,
                dur_min,
            } => {
                let Some(column) = model.days.iter().position(|item| *item == day) else {
                    return;
                };
                let Some(event) = model.events.iter().find(|event| event.id == id) else {
                    return;
                };
                let color = event.color.color(theme);
                (
                    column,
                    start_min,
                    start_min + dur_min,
                    event.title,
                    color.with_alpha(0.55),
                    color,
                )
            }
            TimelineEffect::Select(_) | TimelineEffect::OpenDay(_) => return,
        };
        let block = self.block_rect(area, model.days.len(), geom, column, start_min, end_min);
        fill_round(scene, transform, fill, block, 6.0);
        stroke_round(scene, transform, stroke, block, 6.0, 1.5);
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &block,
        );
        let title = elide_to_width(text, title, 11.0, block.width() - 16.0);
        text.draw_text(
            scene,
            &title,
            11.0,
            colors.neutral[9],
            transform * Affine::translate((block.x0 + 8.0, block.y0 + 4.0)),
            None,
        );
        if block.height() > 30.0 {
            text.draw_text(
                scene,
                &format!("{} – {}", fmt_min(start_min), fmt_min(end_min)),
                10.0,
                colors.text_dim,
                transform * Affine::translate((block.x0 + 8.0, block.y0 + 18.0)),
                None,
            );
        }
        scene.pop_layer();
    }

    fn geometry(area: Rect) -> TimelineGeom {
        TimelineViewport::geometry(area)
    }

    fn column_width(area: Rect, days_len: usize, geom: TimelineGeom) -> f64 {
        TimelineViewport::column_width(area, days_len, geom)
    }

    fn minute_at(&self, geom: TimelineGeom, y: f64) -> i64 {
        self.viewport.minute_at(geom, y)
    }

    fn block_rect(
        &self,
        area: Rect,
        days_len: usize,
        geom: TimelineGeom,
        col: usize,
        start_min: i64,
        end_min: i64,
    ) -> Rect {
        self.viewport
            .block_rect(area, days_len, geom, col, start_min, end_min)
    }

    pub fn layout(&mut self, area: Rect, initial_hour: f64) {
        self.viewport.layout(area, initial_hour);
    }

    pub fn pin_hour(&mut self, hour: f64) {
        self.viewport.pin_hour(hour);
    }

    fn wants_pointer_capture(&self) -> bool {
        self.viewport.wants_pointer_capture()
    }

    fn offset_y(&self) -> f32 {
        self.viewport.offset_y()
    }

    #[cfg(test)]
    pub(super) fn test_set_offset_y(&mut self, offset: f32) {
        self.viewport.set_offset_y(offset);
    }

    #[cfg(test)]
    pub(super) fn test_geometry(area: Rect) -> TimelineGeom {
        Self::geometry(area)
    }

    #[cfg(test)]
    pub(super) fn test_column_width(area: Rect, days_len: usize, geom: TimelineGeom) -> f64 {
        Self::column_width(area, days_len, geom)
    }

    #[cfg(test)]
    pub(super) fn test_offset_y(&self) -> f32 {
        self.offset_y()
    }

    #[cfg(test)]
    pub(super) fn test_block_rect(
        &self,
        area: Rect,
        days_len: usize,
        geom: TimelineGeom,
        column: usize,
        start_min: i64,
        end_min: i64,
    ) -> Rect {
        self.block_rect(area, days_len, geom, column, start_min, end_min)
    }

    #[cfg(test)]
    pub fn vertical_overflows(&self) -> bool {
        self.viewport.vertical_overflows()
    }

    pub fn needs_redraw(&self) -> bool {
        self.viewport.needs_redraw()
    }

    pub fn mark_drawn(&mut self) {
        self.viewport.mark_drawn();
    }

    pub fn needs_animation_frame(&self) -> bool {
        self.viewport.needs_animation_frame()
    }

    pub fn is_layout_dirty(&self) -> bool {
        self.viewport.is_layout_dirty()
    }
}

fn snap_minute(minute: i64) -> i64 {
    ((minute + TIMELINE_SNAP_MIN / 2) / TIMELINE_SNAP_MIN * TIMELINE_SNAP_MIN).clamp(0, 24 * 60)
}

fn fmt_min(minute: i64) -> String {
    let minute = minute.clamp(0, 24 * 60);
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

fn fmt_event_min(minute: i64) -> String {
    let minute = minute.clamp(0, 24 * 60);
    format!("{}:{:02}", minute / 60, minute % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use uuid::Uuid;

    fn area() -> Rect {
        Rect::new(100.0, 40.0, 900.0, 640.0)
    }

    fn days() -> Vec<NaiveDate> {
        let first = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        (0..7)
            .map(|offset| first + Duration::days(offset))
            .collect()
    }

    fn point_at(day_col: usize, minute: i64) -> Point {
        let area = area();
        let geom = TimelineInteraction::geometry(area);
        let col_w = TimelineInteraction::column_width(area, 7, geom);
        Point::new(
            area.x0 + geom.gutter + col_w * (day_col as f64 + 0.5),
            geom.body_top + minute as f64 / 60.0 * geom.hour_h,
        )
    }

    #[test]
    fn create_drag_owns_threshold_day_mapping_and_snap() {
        let mut controller = TimelineInteraction::new();
        controller.test_set_offset_y(0.0);
        let days = days();
        let from = point_at(2, 9 * 60 + 7);
        assert!(controller.begin_drag(area(), &days, &[], from));
        controller.update_drag(Point::new(from.x + 2.0, from.y + 2.0));
        assert!(controller.preview(area(), &days).is_none());
        controller.update_drag(point_at(2, 10 * 60 + 53));
        assert_eq!(
            controller.preview(area(), &days),
            Some(TimelineEffect::Create {
                day: days[2],
                start_min: 9 * 60,
                end_min: 11 * 60,
            })
        );
    }

    #[test]
    fn move_drag_preserves_duration_and_pointer_grab_offset() {
        let mut controller = TimelineInteraction::new();
        controller.test_set_offset_y(0.0);
        let days = days();
        let id = Uuid::new_v4();
        let span = TimelineSpan {
            id,
            day: days[1],
            start_min: 10 * 60,
            end_min: 11 * 60,
        };
        let from = point_at(1, 10 * 60 + 15);
        assert!(controller.begin_drag(area(), &days, &[span], from));
        controller.update_drag(point_at(4, 14 * 60 + 15));
        assert_eq!(
            controller.preview(area(), &days),
            Some(TimelineEffect::Move {
                id,
                day: days[4],
                start_min: 14 * 60,
                dur_min: 60,
            })
        );
    }

    #[test]
    fn pending_event_press_is_a_select_not_a_move() {
        let mut controller = TimelineInteraction::new();
        controller.test_set_offset_y(0.0);
        let days = days();
        let id = Uuid::new_v4();
        let span = TimelineSpan {
            id,
            day: days[0],
            start_min: 8 * 60,
            end_min: 9 * 60,
        };
        let point = point_at(0, 8 * 60 + 30);
        controller.pointer = point;
        assert!(controller.begin_drag(area(), &days, &[span], point));
        assert_eq!(
            controller.finish_drag(area(), &days, &[span]),
            Some(TimelineEffect::Select(id))
        );
    }

    #[test]
    fn cancel_discards_pending_drag_without_an_effect() {
        let mut controller = TimelineInteraction::new();
        let days = days();
        assert!(controller.begin_drag(area(), &days, &[], point_at(3, 12 * 60)));
        assert!(controller.cancel_drag());
        assert!(controller.preview(area(), &days).is_none());
        assert!(!controller.cancel_drag());
    }
}
