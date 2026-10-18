//! Week/day timeline interaction controller.
//!
//! This owns the timeline viewport, pointer/scroll capture, and the complete
//! create/move gesture state machine. It resolves screen coordinates into typed
//! effects; [`CalendarApp`](crate::app::CalendarApp) remains responsible for
//! applying those effects to the calendar model and CRDT.

use crate::input_context::InputContext;
use chrono::{Datelike, NaiveDate};
use vello::kurbo::{Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::dnd::DRAG_START_THRESHOLD_PX;
use ui::paint::PaintCtx;

use crate::date;
use crate::draw_util::{
    disc, hairline, line_height, stroke_line, text_centered, text_in_band, wrap_lines,
};
use crate::model::{Id, Palette};
use crate::timeline_viewport::{TimelineGeom, TimelineType, TimelineViewport};
use ui::surface::card;
use ui::tokens::colors::SMALL_TEXT_MIN_CONTRAST;

pub(super) const TIMELINE_SNAP_MIN: i64 = 15;

/// Where an event's block sits across its day's column: overlapping events
/// share the column side by side, each in its own lane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Lane {
    pub index: usize,
    pub of: usize,
}

/// The lanes of a day's timed events, `[start, end)` minutes each, in the
/// same order. Events that overlap, directly or through others, form a
/// cluster that splits the column evenly; each takes the leftmost lane free
/// when it starts.
pub(super) fn lanes(intervals: &[(i64, i64)]) -> Vec<Lane> {
    let mut order: Vec<(usize, i64, i64)> = intervals
        .iter()
        .enumerate()
        .map(|(i, &(start, end))| (i, start, end.max(start + 1)))
        .collect();
    order.sort_by_key(|&(_, start, end)| (start, -end));
    let mut placed: Vec<(usize, Lane)> = Vec::with_capacity(order.len());
    let mut lane_ends: Vec<i64> = Vec::new();
    let mut cluster_start = 0;
    let mut cluster_end = i64::MIN;
    let close = |placed: &mut [(usize, Lane)], from: usize, of: usize| {
        for (_, lane) in placed.iter_mut().skip(from) {
            lane.of = of.max(1);
        }
    };
    for (i, start, end) in order {
        if start >= cluster_end && placed.len() > cluster_start {
            close(&mut placed, cluster_start, lane_ends.len());
            cluster_start = placed.len();
            lane_ends.clear();
        }
        let index = if let Some((free, slot)) = lane_ends
            .iter_mut()
            .enumerate()
            .find(|(_, lane_end)| **lane_end <= start)
        {
            *slot = end;
            free
        } else {
            lane_ends.push(end);
            lane_ends.len() - 1
        };
        cluster_end = if placed.len() == cluster_start {
            end
        } else {
            cluster_end.max(end)
        };
        placed.push((i, Lane { index, of: 1 }));
    }
    close(&mut placed, cluster_start, lane_ends.len());
    placed.sort_by_key(|&(i, _)| i);
    placed.into_iter().map(|(_, lane)| lane).collect()
}

/// How far up from a block's bottom edge a press resizes it instead of
/// moving it, in logical pixels.
const RESIZE_GRIP: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TimelineSpan {
    pub id: Id,
    /// The occurrence's date in its series.
    pub original: NaiveDate,
    pub day: NaiveDate,
    pub start_min: i64,
    pub end_min: i64,
    pub lane: Lane,
}

pub(super) struct TimelineRenderEvent<'a> {
    pub id: Id,
    pub day: NaiveDate,
    pub title: &'a str,
    pub location: &'a str,
    pub color: Palette,
    pub start_min: i64,
    pub end_min: i64,
    /// Authored wall endpoint, unlike the elapsed-time geometry endpoint.
    pub clock_end_min: i64,
    pub all_day: bool,
    pub lane: Lane,
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
        original: NaiveDate,
        day: NaiveDate,
        start_min: i64,
        dur_min: i64,
    },
    /// A new end for the occurrence, on the day it starts.
    Resize {
        id: Id,
        original: NaiveDate,
        day: NaiveDate,
        start_min: i64,
        end_min: i64,
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
        original: NaiveDate,
        origin: Point,
        current: Point,
        grab_off_min: i64,
        dur_min: i64,
        active: bool,
    },
    Resize {
        span: TimelineSpan,
        origin: Point,
        current: Point,
        active: bool,
    },
}

impl TimelineDrag {
    fn is_active(self) -> bool {
        match self {
            Self::Create { active, .. }
            | Self::Move { active, .. }
            | Self::Resize { active, .. } => active,
        }
    }

    fn origin(self) -> Point {
        match self {
            Self::Create { origin, .. }
            | Self::Move { origin, .. }
            | Self::Resize { origin, .. } => origin,
        }
    }

    fn set_current(&mut self, point: Point) {
        match self {
            Self::Create { current, .. }
            | Self::Move { current, .. }
            | Self::Resize { current, .. } => *current = point,
        }
    }

    fn set_active(&mut self) {
        match self {
            Self::Create { active, .. }
            | Self::Move { active, .. }
            | Self::Resize { active, .. } => {
                *active = true;
            }
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
        cx: &mut InputContext<'_>,
        area: Rect,
        days: &[NaiveDate],
        spans: &[TimelineSpan],
    ) -> TimelineOutcome {
        if let WindowEvent::CursorMoved { position, .. } = cx.event {
            self.pointer = Point::new(position.x / cx.scale_factor, position.y / cx.scale_factor);
        }

        match cx.event {
            WindowEvent::MouseWheel { .. } if area.contains(self.pointer) => {
                let handled = self.forward_to_scroll(cx);
                TimelineOutcome {
                    handled,
                    effect: None,
                }
            }
            WindowEvent::CursorMoved { .. } => self.handle_pointer_move(cx, area),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let body = self.geometry(area).body(area.x0, area.x1);
                if !body.contains(self.pointer) {
                    return TimelineOutcome::ignored();
                }
                if self.forward_to_scroll(cx) {
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
                    let handled = self.forward_to_scroll(cx);
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

    fn handle_pointer_move(&mut self, cx: &mut InputContext<'_>, area: Rect) -> TimelineOutcome {
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
        let geom = self.geometry(area);
        if let Some(span) = self.event_at(area, days, spans, point) {
            if self.on_resize_grip(area, days.len(), span, point) {
                self.drag = Some(TimelineDrag::Resize {
                    span,
                    origin: point,
                    current: point,
                    active: false,
                });
                return true;
            }
            let press_min = self.minute_at(geom, point.y);
            self.drag = Some(TimelineDrag::Move {
                id: span.id,
                original: span.original,
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
        let geom = self.geometry(area);
        if point.x < area.x0 + geom.gutter || point.x > area.x1 {
            return None;
        }
        let col = TimelineViewport::column_at(area, days.len(), geom, point.x);
        let day = *days.get(col)?;
        spans.iter().rev().copied().find(|span| {
            span.day == day && self.span_rect(area, days.len(), col, span).contains(point)
        })
    }

    /// A span's block in column `col`, in its lane.
    fn span_rect(&self, area: Rect, days_len: usize, col: usize, span: &TimelineSpan) -> Rect {
        let geom = self.geometry(area);
        let block = self.block_rect(area, days_len, geom, col, span.start_min, span.end_min);
        in_lane(block, span.lane)
    }

    /// Whether `point` is on the bottom edge of `span`'s block, where a
    /// press resizes it. A block too short to hold both a grip and a body
    /// only moves.
    fn on_resize_grip(
        &self,
        area: Rect,
        days_len: usize,
        span: TimelineSpan,
        point: Point,
    ) -> bool {
        let geom = self.geometry(area);
        let col = TimelineViewport::column_at(area, days_len, geom, point.x);
        let block = self.span_rect(area, days_len, col, &span);
        block.height() >= 3.0 * RESIZE_GRIP
            && point.y >= block.y1 - RESIZE_GRIP
            && point.y <= block.y1
    }

    /// Whether `point` is on an event's resize grip: the pointer shows the
    /// vertical resize shape there.
    pub fn resize_grip_at(
        &self,
        area: Rect,
        days: &[NaiveDate],
        spans: &[TimelineSpan],
        point: Point,
    ) -> bool {
        if let Some(TimelineDrag::Resize { .. }) = self.drag {
            return true;
        }
        self.event_at(area, days, spans, point)
            .is_some_and(|span| self.on_resize_grip(area, days.len(), span, point))
    }

    fn effect_for_drag(
        &self,
        area: Rect,
        days: &[NaiveDate],
        drag: TimelineDrag,
    ) -> Option<TimelineEffect> {
        let geom = self.geometry(area);
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
                original,
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
                    original,
                    day: *days.get(col)?,
                    start_min,
                    dur_min,
                })
            }
            TimelineDrag::Resize { span, current, .. } => {
                let end_min = snap_minute(self.minute_at(geom, current.y))
                    .clamp(span.start_min + TIMELINE_SNAP_MIN, 24 * 60);
                Some(TimelineEffect::Resize {
                    id: span.id,
                    original: span.original,
                    day: span.day,
                    start_min: span.start_min,
                    end_min,
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
        let body = self.geometry(area).body(area.x0, area.x1);
        body.contains(self.pointer)
            .then(|| self.event_at(area, days, spans, self.pointer))
            .flatten()
            .map(|span| TimelineEffect::Select(span.id))
    }

    fn forward_to_scroll(&mut self, cx: &mut InputContext<'_>) -> bool {
        self.viewport.handle_event(cx)
    }

    fn preview(&self, area: Rect, days: &[NaiveDate]) -> Option<TimelineEffect> {
        let drag = self.drag.filter(|drag| drag.is_active())?;
        self.effect_for_drag(area, days, drag)
    }

    /// The event a drag is moving or resizing, whose block the ghost
    /// stands in for.
    fn moving_id(&self) -> Option<Id> {
        match self.drag {
            Some(TimelineDrag::Move {
                id, active: true, ..
            }) => Some(id),
            Some(TimelineDrag::Resize {
                span, active: true, ..
            }) => Some(span.id),
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
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        model: &TimelineRenderModel<'_, '_>,
    ) -> Vec<(Rect, TimelineEffect)> {
        let theme = ctx.theme;
        let ground = theme.tokens.colors.surfaces.content;
        let mut hits = Vec::new();
        let ty = TimelineType::of(theme);

        let geom = self.geometry(area);
        Self::draw_headers(ctx, &ty, area, model, geom, &mut hits);
        hairline(
            ctx.scene,
            ctx.transform,
            ground,
            Point::new(area.x0, geom.body_top),
            Point::new(geom.grid_right, geom.body_top),
        );

        let body = geom.body(area.x0, area.x1);
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        self.draw_grid(ctx, &ty, area, model, geom);
        self.draw_events(ctx, &ty, area, model, &mut hits);
        self.draw_drag_ghost(ctx, &ty, area, model, geom);
        ctx.scene.pop_layer();
        self.viewport.draw(ctx.scene, ctx.transform, theme);
        hits
    }

    fn draw_headers(
        ctx: &mut PaintCtx<'_>,
        ty: &TimelineType,
        area: Rect,
        model: &TimelineRenderModel<'_, '_>,
        geom: TimelineGeom,
        hits: &mut Vec<(Rect, TimelineEffect)>,
    ) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let sp = &theme.tokens.spacing;
        let (s1, s2) = (f64::from(sp.s1), f64::from(sp.s2));
        let grid_x0 = area.x0 + geom.gutter;
        let col_w = Self::column_width(area, model.days.len(), geom);
        // Weekday label, then the day number directly beneath it: both rows
        // come from the measured lines the header height was solved from.
        let weekday_h = line_height(ctx, &ty.weekday);
        let day_h = line_height(ctx, &ty.day);
        let weekday_y = area.y0 + s2;
        let day_y = weekday_y + weekday_h;
        for (index, day) in model.days.iter().enumerate() {
            let x0 = grid_x0 + col_w * index as f64;
            let header = Rect::new(x0, area.y0, x0 + col_w, area.y0 + geom.head_h);
            let is_today = *day == model.today;
            let weekend = day.weekday().num_days_from_monday() >= 5;
            let text_x = x0 + s2;
            let weekday = date::weekday_short(*day);
            ctx.text(
                &weekday,
                &ty.weekday,
                if weekend {
                    colors.text_dim
                } else {
                    colors.text_primary
                },
                Point::new(text_x, weekday_y),
                None,
            );
            let number = day.day().to_string();
            if is_today {
                // Today's number stands on the inverted disc, as in the
                // month grid.
                let number_w = f64::from(ctx.text.measure_style(&number, None, &ty.day).0);
                let side = day_h.max(number_w + s2);
                let disc = Rect::new(text_x - s1, day_y, text_x - s1 + side, day_y + day_h);
                ctx.fill_round(disc, day_h / 2.0, colors.intent.inverted);
                text_centered(ctx, &number, &ty.day, colors.intent.on_inverted, disc);
            } else {
                ctx.text(
                    &number,
                    &ty.day,
                    if weekend {
                        colors.text_dim
                    } else {
                        colors.text_primary
                    },
                    Point::new(text_x, day_y),
                    None,
                );
            }
            hits.push((header, TimelineEffect::OpenDay(*day)));

            let all_day: Vec<_> = model
                .events
                .iter()
                .filter(|event| event.day == *day && event.all_day)
                .collect();
            let rows = geom.allday_rows;
            let chip_h = if rows <= 1 {
                geom.allday_h - 2.0 * s1
            } else {
                line_height(ctx, &ty.strip)
            };
            for (row, event) in all_day.iter().enumerate().take(rows) {
                let top = area.y0 + geom.head_h + s1 + row as f64 * (chip_h + s1);
                let strip = Rect::new(
                    x0 + geom.block_inset,
                    top,
                    x0 + col_w - geom.block_inset,
                    top + chip_h,
                );
                // The last row counts what does not fit.
                if row + 1 == rows && all_day.len() > rows {
                    let more = format!("+{} more", all_day.len() - row);
                    text_in_band(
                        ctx,
                        &more,
                        &ty.strip,
                        colors.text_dim,
                        strip.x0 + s1 + s1 / 2.0,
                        strip,
                        strip.width() - 3.0 * s1,
                    );
                    hits.push((strip, TimelineEffect::OpenDay(*day)));
                    break;
                }
                let density = if rows <= 1 {
                    geom.density
                } else {
                    ui::widget::TimelineDensity::Regular
                };
                Self::draw_all_day_chip(ctx, ty, strip, event, density);
                hits.push((strip, TimelineEffect::Select(event.id)));
            }
        }
    }

    /// An all-day event's chip in its day's strip: a deeper wash of the
    /// calendar colour, its title wrapped over the lines the density gives
    /// the strip and centred as one block.
    fn draw_all_day_chip(
        ctx: &mut PaintCtx<'_>,
        ty: &TimelineType,
        strip: Rect,
        event: &TimelineRenderEvent<'_>,
        density: ui::widget::TimelineDensity,
    ) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let s1 = f64::from(theme.tokens.spacing.s1);
        let tinted = colors.tinted_chip_on(
            colors.surfaces.content,
            event.color.color(theme),
            crate::draw_util::ALL_DAY_WASH,
            SMALL_TEXT_MIN_CONTRAST,
        );
        card(
            ctx.scene,
            ctx.transform,
            strip,
            f64::from(theme.tokens.radii.detail),
            tinted.fill,
        );
        let width = strip.width() - 3.0 * s1;
        let lines = wrap_lines(ctx, event.title, &ty.strip, width, density.chip_lines());
        let strip_h = line_height(ctx, &ty.strip);
        #[allow(clippy::cast_precision_loss, reason = "at most two lines")]
        let block_h = strip_h * lines.len() as f64;
        let mut y = strip.center().y - block_h / 2.0;
        for line in &lines {
            let band = Rect::new(strip.x0, y, strip.x1, y + strip_h);
            text_in_band(
                ctx,
                line,
                &ty.strip,
                tinted.ink,
                strip.x0 + s1 + s1 / 2.0,
                band,
                width,
            );
            y += strip_h;
        }
    }

    fn draw_grid(
        &self,
        ctx: &mut PaintCtx<'_>,
        ty: &TimelineType,
        area: Rect,
        model: &TimelineRenderModel<'_, '_>,
        geom: TimelineGeom,
    ) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let ground = colors.surfaces.content;
        let s1 = f64::from(theme.tokens.spacing.s1);
        let s3 = f64::from(theme.tokens.spacing.s3);
        let grid_x0 = area.x0 + geom.gutter;
        let col_w = Self::column_width(area, model.days.len(), geom);
        let offset = f64::from(self.offset_y());
        let y_at = |minute: f64| geom.body_top + minute / 60.0 * geom.hour_h - offset;
        let body = geom.body(area.x0, area.x1);
        for hour in 0..=24 {
            let y = y_at(f64::from(hour) * 60.0);
            hairline(
                ctx.scene,
                ctx.transform,
                ground,
                Point::new(grid_x0, y),
                Point::new(geom.grid_right, y),
            );
            if hour < 24 {
                ctx.text(
                    &format!("{hour:02}:00"),
                    &ty.hour,
                    colors.text_dim,
                    Point::new(area.x0 + s3, y + s1 / 2.0),
                    None,
                );
            }
        }
        for index in 0..=model.days.len() {
            let x = grid_x0 + col_w * index as f64;
            hairline(
                ctx.scene,
                ctx.transform,
                ground,
                Point::new(x, body.y0),
                Point::new(x, body.y1),
            );
        }
        if let Some((now_day, now_min)) = model.now
            && let Some(index) = model.days.iter().position(|day| *day == now_day)
        {
            // The current time is live state: the status colour, a rule
            // across today's column with a dot where it starts.
            let y = y_at(now_min as f64);
            let x0 = grid_x0 + col_w * index as f64;
            let live = colors.semantic.danger;
            stroke_line(
                ctx.scene,
                ctx.transform,
                live,
                Point::new(x0, y),
                Point::new(x0 + col_w, y),
                s1 / 2.0,
            );
            disc(ctx, Point::new(x0, y), s1, live);
        }
    }

    fn draw_events(
        &self,
        ctx: &mut PaintCtx<'_>,
        ty: &TimelineType,
        area: Rect,
        model: &TimelineRenderModel<'_, '_>,
        hits: &mut Vec<(Rect, TimelineEffect)>,
    ) {
        let theme = ctx.theme;
        let geom = self.geometry(area);
        let body = geom.body(area.x0, area.x1);
        let colors = &theme.tokens.colors;
        let s1 = f64::from(theme.tokens.spacing.s1);
        let radius = f64::from(theme.tokens.radii.item);
        let moving_id = self.moving_id();
        for (index, day) in model.days.iter().enumerate() {
            for event in model
                .events
                .iter()
                .filter(|event| event.day == *day && !event.all_day && Some(event.id) != moving_id)
            {
                let block = in_lane(
                    self.block_rect(
                        area,
                        model.days.len(),
                        geom,
                        index,
                        event.start_min,
                        event.end_min,
                    ),
                    event.lane,
                );
                let color = event.color.color(theme);
                // The block is a wash of the calendar colour over the grid,
                // with the colour itself as a bar down its leading edge.
                let tinted = colors.tinted_chip_on(
                    colors.surfaces.content,
                    color,
                    0.22,
                    SMALL_TEXT_MIN_CONTRAST,
                );
                card(ctx.scene, ctx.transform, block, radius, tinted.fill);
                ctx.scene.push_layer(
                    Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    ctx.transform,
                    &vello::kurbo::RoundedRect::from_rect(block, radius),
                );
                ctx.fill_rect(
                    Rect::new(block.x0, block.y0, block.x0 + s1, block.y1),
                    color,
                );
                let text_x = block.x0 + 2.0 * s1;
                let text_w = (block.x1 - s1 - text_x).max(0.0);
                // The title comes first and wraps over every line the block
                // has room for; the time and then the location follow on
                // lines of their own while room is left. The block's place
                // in the grid already tells the time.
                let title_h = line_height(ctx, &ty.title);
                let subtitle_h = line_height(ctx, &ty.subtitle);
                // A block that began above the scrolled-to hours keeps its
                // title at the top of what shows of it.
                let top = block.y0.max(body.y0.min(block.y1 - title_h - 2.0 * s1));
                let room = block.y1 - top - 2.0 * s1;
                let max_lines = ((room / title_h).floor() as usize).max(1);
                let mut y = top + s1;
                for line in wrap_lines(ctx, event.title, &ty.title, text_w, max_lines) {
                    ctx.text(&line, &ty.title, tinted.ink, Point::new(text_x, y), None);
                    y += title_h;
                }
                let time = ui::format::range(
                    &fmt_event_min(event.start_min),
                    &fmt_event_min(event.clock_end_min),
                );
                for line in [time.as_str(), event.location] {
                    if line.is_empty() || y + subtitle_h > block.y1 - s1 + 0.5 {
                        break;
                    }
                    let line = ctx.text.elide_to_width_style(
                        line,
                        ui::num::f32_of_f64(text_w),
                        &ty.subtitle,
                    );
                    ctx.text(&line, &ty.subtitle, tinted.ink, Point::new(text_x, y), None);
                    y += subtitle_h;
                }
                ctx.scene.pop_layer();
                let visible = block.intersect(body);
                if visible.width() > 0.0 && visible.height() > 0.0 {
                    hits.push((visible, TimelineEffect::Select(event.id)));
                }
            }
        }
    }

    fn draw_drag_ghost(
        &self,
        ctx: &mut PaintCtx<'_>,
        ty: &TimelineType,
        area: Rect,
        model: &TimelineRenderModel<'_, '_>,
        geom: TimelineGeom,
    ) {
        let theme = ctx.theme;
        let Some(preview) = self.preview(area, model.days) else {
            return;
        };
        let Some(Ghost {
            column,
            start_min,
            end_min,
            title,
            fill,
            edge,
        }) = Self::ghost(theme, model, preview)
        else {
            return;
        };
        let s1 = f64::from(theme.tokens.spacing.s1);
        let radius = f64::from(theme.tokens.radii.item);
        let block = self.block_rect(area, model.days.len(), geom, column, start_min, end_min);
        ctx.fill_round(block, radius, fill);
        ctx.stroke_round(block, radius, edge, s1 / 2.0);
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &block,
        );
        let ink = theme.text_primary;
        let text_x = block.x0 + 2.0 * s1;
        let title = ctx.text.elide_to_width_style(
            title,
            ui::num::f32_of_f64((block.x1 - s1 - text_x).max(0.0)),
            &ty.title,
        );
        let title_h = line_height(ctx, &ty.title);
        let subtitle_h = line_height(ctx, &ty.subtitle);
        ctx.text(
            &title,
            &ty.title,
            ink,
            Point::new(text_x, block.y0 + s1),
            None,
        );
        if block.height() > s1 + title_h + subtitle_h {
            ctx.text(
                &ui::format::range(&fmt_min(start_min), &fmt_min(end_min)),
                &ty.subtitle,
                ink,
                Point::new(text_x, block.y0 + s1 + title_h),
                None,
            );
        }
        ctx.scene.pop_layer();
    }

    /// What the drag ghost shows for `preview`: its column, minutes, title
    /// and colours.
    fn ghost<'a>(
        theme: &ui::theme::Theme,
        model: &'a TimelineRenderModel<'_, '_>,
        preview: TimelineEffect,
    ) -> Option<Ghost<'a>> {
        let colors = &theme.tokens.colors;
        let (column, start_min, end_min, title, fill, edge) = match preview {
            TimelineEffect::Create {
                day,
                start_min,
                end_min,
            } => {
                let column = model.days.iter().position(|item| *item == day)?;
                // A new event being drawn is your own selection.
                (
                    column,
                    start_min,
                    end_min,
                    model.untitled,
                    colors.intent.selection_tint,
                    colors.intent.you,
                )
            }
            TimelineEffect::Move {
                id,
                day,
                start_min,
                dur_min,
                ..
            }
            | TimelineEffect::Resize {
                id,
                day,
                start_min,
                end_min: dur_min,
                ..
            } => {
                let dur_min = if matches!(preview, TimelineEffect::Resize { .. }) {
                    dur_min - start_min
                } else {
                    dur_min
                };
                let column = model.days.iter().position(|item| *item == day)?;
                let event = model.events.iter().find(|event| event.id == id)?;
                let color = event.color.color(theme);
                let tinted = colors.tinted_chip_on(
                    colors.surfaces.content,
                    color,
                    0.4,
                    SMALL_TEXT_MIN_CONTRAST,
                );
                (
                    column,
                    start_min,
                    start_min + dur_min,
                    event.title,
                    tinted.fill,
                    color,
                )
            }
            TimelineEffect::Select(_) | TimelineEffect::OpenDay(_) => return None,
        };
        Some(Ghost {
            column,
            start_min,
            end_min,
            title,
            fill,
            edge,
        })
    }

    /// Geometry at the viewport's measured type, so paint, hit-testing and
    /// drag mapping all read one description of the header / strip / body.
    fn geometry(&self, area: Rect) -> TimelineGeom {
        self.viewport.current_geometry(area)
    }

    /// Measure the theme's type so header, strip and block heights grow
    /// with the type they carry. Called from the app's layout pass.
    pub fn set_layout_theme(&mut self, theme: &ui::theme::Theme) {
        self.viewport.set_layout_theme(theme);
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

    /// Lay out `days_len` columns under an all-day strip of `allday_rows`
    /// chips.
    pub fn layout(&mut self, area: Rect, initial_hour: f64, days_len: usize, allday_rows: usize) {
        self.viewport
            .layout(area, initial_hour, (days_len, allday_rows));
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
    pub(super) fn test_layout_geometry(&self, area: Rect) -> TimelineGeom {
        self.geometry(area)
    }

    #[cfg(test)]
    pub(super) fn test_geometry(area: Rect) -> TimelineGeom {
        TimelineViewport::geometry(area)
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

/// The drag ghost's content.
struct Ghost<'a> {
    column: usize,
    start_min: i64,
    end_min: i64,
    title: &'a str,
    fill: vello::peniko::Color,
    edge: vello::peniko::Color,
}

/// `block` narrowed to its lane of the column, with a hairline between
/// neighbours.
fn in_lane(block: Rect, lane: Lane) -> Rect {
    if lane.of <= 1 {
        return block;
    }
    let width = block.width() / lane.of as f64;
    let x0 = block.x0 + width * lane.index as f64;
    let gap = if lane.index + 1 < lane.of { 1.0 } else { 0.0 };
    Rect::new(x0, block.y0, x0 + width - gap, block.y1)
}

fn snap_minute(minute: i64) -> i64 {
    ((minute + TIMELINE_SNAP_MIN / 2) / TIMELINE_SNAP_MIN * TIMELINE_SNAP_MIN).clamp(0, 24 * 60)
}

/// A minute of the day as the person reads clocks.
fn fmt_min(minute: i64) -> String {
    let minute = u32::try_from(minute.clamp(0, 24 * 60)).unwrap_or(0);
    ui::clock::hour_minute(minute / 60, minute % 60)
}

fn fmt_event_min(minute: i64) -> String {
    fmt_min(minute)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use uuid::Uuid;

    fn area() -> Rect {
        Rect::new(100.0, 40.0, 1300.0, 640.0)
    }

    fn days() -> Vec<NaiveDate> {
        let first = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        (0..7)
            .map(|offset| first + Duration::days(offset))
            .collect()
    }

    fn point_at(day_col: usize, minute: i64) -> Point {
        let area = area();
        let geom = TimelineInteraction::test_geometry(area);
        let col_w = TimelineInteraction::column_width(area, 7, geom);
        Point::new(
            area.x0 + geom.gutter + col_w * (day_col as f64 + 0.5),
            geom.body_top + minute as f64 / 60.0 * geom.hour_h,
        )
    }

    #[test]
    fn drag_gestures_own_threshold_snap_grab_offset_select_and_cancel() {
        // Case `create_drag_owns_threshold_day_mapping_and_snap`.
        {
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
        // Case `move_drag_preserves_duration_and_pointer_grab_offset`.
        {
            let mut controller = TimelineInteraction::new();
            controller.test_set_offset_y(0.0);
            let days = days();
            let id = Uuid::new_v4();
            let span = TimelineSpan {
                id,
                original: days[1],
                day: days[1],
                start_min: 10 * 60,
                end_min: 11 * 60,
                lane: Lane::default(),
            };
            let from = point_at(1, 10 * 60 + 15);
            assert!(controller.begin_drag(area(), &days, &[span], from));
            controller.update_drag(point_at(4, 14 * 60 + 15));
            assert_eq!(
                controller.preview(area(), &days),
                Some(TimelineEffect::Move {
                    id,
                    original: days[1],
                    day: days[4],
                    start_min: 14 * 60,
                    dur_min: 60,
                })
            );
        }
        // Case `pending_event_press_is_a_select_not_a_move`.
        {
            let mut controller = TimelineInteraction::new();
            controller.test_set_offset_y(0.0);
            let days = days();
            let id = Uuid::new_v4();
            let span = TimelineSpan {
                id,
                original: days[0],
                day: days[0],
                start_min: 8 * 60,
                end_min: 9 * 60,
                lane: Lane::default(),
            };
            let point = point_at(0, 8 * 60 + 30);
            controller.pointer = point;
            assert!(controller.begin_drag(area(), &days, &[span], point));
            assert_eq!(
                controller.finish_drag(area(), &days, &[span]),
                Some(TimelineEffect::Select(id))
            );
        }
        // Case `cancel_discards_pending_drag_without_an_effect`.
        {
            let mut controller = TimelineInteraction::new();
            let days = days();
            assert!(controller.begin_drag(area(), &days, &[], point_at(3, 12 * 60)));
            assert!(controller.cancel_drag());
            assert!(controller.preview(area(), &days).is_none());
            assert!(!controller.cancel_drag());
        }
    }

    #[test]
    fn dragging_an_events_bottom_edge_changes_only_its_end() {
        let mut controller = TimelineInteraction::new();
        controller.test_set_offset_y(0.0);
        let days = days();
        let id = Uuid::new_v4();
        let span = TimelineSpan {
            id,
            original: days[2],
            day: days[2],
            start_min: 9 * 60,
            end_min: 10 * 60,
            lane: Lane::default(),
        };
        // Just above the bottom edge is the grip.
        let mut grip = point_at(2, 10 * 60);
        grip.y -= 2.0;
        assert!(controller.resize_grip_at(area(), &days, &[span], grip));
        assert!(!controller.resize_grip_at(area(), &days, &[span], point_at(2, 9 * 60 + 30)));
        assert!(controller.begin_drag(area(), &days, &[span], grip));
        controller.update_drag(point_at(5, 11 * 60 + 40));
        assert_eq!(
            controller.preview(area(), &days),
            Some(TimelineEffect::Resize {
                id,
                original: days[2],
                day: days[2],
                start_min: 9 * 60,
                end_min: 11 * 60 + 45,
            }),
            "the day stays; the end snaps to a quarter hour"
        );
        // An end above the start is the shortest event instead.
        controller.update_drag(point_at(2, 8 * 60));
        assert!(matches!(
            controller.preview(area(), &days),
            Some(TimelineEffect::Resize { end_min, .. }) if end_min == 9 * 60 + 15
        ));
    }

    #[test]
    fn overlapping_events_share_their_day_side_by_side() {
        let lane = |index, of| Lane { index, of };
        // 9–10 and 9:30–11 overlap; 10–10:30 fits beside 9:30–11 in the
        // first lane; 12–13 stands alone.
        assert_eq!(
            lanes(&[(540, 600), (570, 660), (600, 630), (720, 780)]),
            vec![lane(0, 2), lane(1, 2), lane(0, 2), lane(0, 1)]
        );
        // Three at once take three lanes.
        assert_eq!(
            lanes(&[(540, 600), (540, 600), (540, 600)]),
            vec![lane(0, 3), lane(1, 3), lane(2, 3)]
        );
        // Back to back is no overlap.
        assert_eq!(
            lanes(&[(540, 600), (600, 660)]),
            vec![lane(0, 1), lane(0, 1)]
        );
        // A block hit lands in its own lane.
        let mut controller = TimelineInteraction::new();
        controller.test_set_offset_y(0.0);
        let days = days();
        let (left, right) = (Uuid::new_v4(), Uuid::new_v4());
        let span = |id, lane| TimelineSpan {
            id,
            original: days[0],
            day: days[0],
            start_min: 9 * 60,
            end_min: 10 * 60,
            lane,
        };
        let spans = [span(left, lane(0, 2)), span(right, lane(1, 2))];
        let mid = point_at(0, 9 * 60 + 30);
        let geom = TimelineInteraction::test_geometry(area());
        let quarter = TimelineInteraction::column_width(area(), 7, geom) / 4.0;
        let hit = |x: f64| {
            controller
                .event_at(area(), &days, &spans, Point::new(x, mid.y))
                .map(|span| span.id)
        };
        assert_eq!(hit(mid.x - quarter), Some(left));
        assert_eq!(hit(mid.x + quarter), Some(right));
    }
}
