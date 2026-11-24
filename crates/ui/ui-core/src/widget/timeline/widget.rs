//! [`Timeline`], the retained widget: a day, week or month grid that draws with the shared faces
//! and never changes the data it shows. A drag moves, resizes or creates an item and raises the
//! typed event that says so; the view decides what the change is.

use std::collections::HashMap;

use jiff::civil::{Date, DateTime};
use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::component::{Event, Value};
use crate::motion::{Direction, MotionValue};
use crate::paint::PaintCtx;

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::MotionSource;
use crate::widget::{Scrollable, TaffyTreeExt, Widget};

use super::Ghost;
use super::face::{MonthFace, TimelineFace};
use super::{
    MINUTES_PER_DAY, Split, TimelineGeom, TimelineItem, TimelineMode, TimelineTokens, block_rect,
    column_at, geometry, in_split, minute_at, natural_height, snap_minute, splits_for,
};

const DRAG_THRESHOLD: f64 = crate::dnd::DRAG_START_THRESHOLD_PX;

/// A time grid: one day, a week, or a month, at 10k events as at three (only what the viewport
/// shows is laid out and painted).
pub struct Timeline {
    pub(crate) mode: TimelineMode,
    pub(crate) anchor: Date,
    pub(crate) days: Vec<Date>,
    pub(crate) today: Date,
    pub(crate) show_now: bool,
    pub(crate) items: Vec<TimelineItem>,
    pub(crate) item_events: Vec<ItemEvents>,
    pub(crate) slot: i64,
    pub(crate) zoom: f64,
    pub(crate) min_rows: usize,
    pub(crate) allday_rows: usize,
    pub(crate) label: Option<String>,
    pub(crate) created: Event,
    pub(crate) open_day: Event,
    pub(crate) zoomed: Event,
    theme: Option<Theme>,
    tokens: Option<TimelineTokens>,
    rect: Rect,
    dirty: bool,
    layout_dirty: bool,
    pointer: Point,
    focus: Option<usize>,
    drag: Option<Drag>,
    slide: MotionValue<f64>,
    slide_pending: Option<f64>,
    /// The events last painted, so a view's own interests and a test can read them.
    last_painted: usize,
    /// The y the now line was last drawn at, so a wake repaints only when it crosses a pixel row.
    now_y: Option<f64>,
    color_theme: Option<String>,
    scroll: Scrollable,
    seeded: bool,
}

#[derive(Clone, Copy, Debug)]
enum DragKind {
    Create {
        day: Date,
        origin: Point,
        body: bool,
    },
    Move {
        index: usize,
        grab_off: i64,
    },
    Resize {
        index: usize,
    },
}

/// What one item's own handlers are: a press, and a drag that moves or resizes it.
#[derive(Clone, Default)]
pub(crate) struct ItemEvents {
    pub press: Event,
    pub moved: Event,
    pub resized: Event,
}

#[derive(Clone, Debug)]
struct Drag {
    kind: DragKind,
    origin: Point,
    current: Point,
    active: bool,
}

/// The change a drag names when it lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Create {
        day: Date,
        start_min: i64,
        end_min: i64,
    },
    Move {
        index: usize,
        day: Date,
        start_min: i64,
    },
    Resize {
        index: usize,
        end_min: i64,
    },
}

impl Timeline {
    pub(crate) fn new(mode: TimelineMode, anchor: Date, items: Vec<TimelineItem>) -> Self {
        let today = crate::clock::today();
        let days = period_days(mode, anchor);
        let rows = super::all_day_rows(&items);
        Self {
            mode,
            anchor,
            days,
            today,
            show_now: true,
            item_events: vec![ItemEvents::default(); items.len()],
            items,
            slot: super::SNAP_MIN,
            zoom: 1.0,
            min_rows: 0,
            allday_rows: rows,
            label: None,
            created: Event::None,
            open_day: Event::None,
            zoomed: Event::None,
            theme: None,
            tokens: None,
            rect: Rect::ZERO,
            dirty: true,
            layout_dirty: true,
            pointer: Point::ZERO,
            focus: None,
            drag: None,
            slide: MotionValue::new(0.0),
            slide_pending: None,
            last_painted: 0,
            now_y: None,
            color_theme: None,
            scroll: Scrollable::new_vertical().always_visible(),
            seeded: false,
        }
    }

    /// Show the period `anchor` stands in, keeping what the person was doing.
    pub(crate) fn retarget(
        &mut self,
        mode: TimelineMode,
        anchor: Date,
        items: Vec<TimelineItem>,
        forward: bool,
    ) {
        let days = period_days(mode, anchor);
        if mode != self.mode || self.days != days {
            self.slide_pending = Some(if forward { 1.0 } else { -1.0 });
        }
        self.mode = mode;
        self.anchor = anchor;
        self.days = days;
        self.items = items;
        self.item_events = vec![ItemEvents::default(); self.items.len()];
        self.allday_rows = super::all_day_rows(&self.items).max(self.min_rows);
        self.layout_dirty = true;
        self.dirty = true;
    }

    pub(crate) fn theme_for_props(&self) -> Option<Theme> {
        self.theme.clone()
    }

    fn resolve_colors(&mut self, theme: &Theme) {
        if self.color_theme.as_deref() == Some(theme.name.as_str()) {
            return;
        }
        for item in &mut self.items {
            item.color = item.tone.color(theme);
        }
        self.color_theme = Some(theme.name.clone());
    }

    fn start_slide(&mut self, theme: &Theme) {
        if let Some(sign) = self.slide_pending.take() {
            self.slide.snap(sign);
            let timing = crate::motion::CROSS_FADE.timing(
                Direction::Enter,
                theme.motion(),
                MotionSource::Pointer,
            );
            self.slide.set(0.0, timing, crate::motion::frame_now());
        }
    }

    fn tokens(&self) -> TimelineTokens {
        self.tokens.clone().unwrap_or_default()
    }

    /// Where the now line stands, when it is shown and this grid holds today.
    fn now_here(&self) -> Option<(Date, i64)> {
        if !self.show_now || self.mode == TimelineMode::Month {
            return None;
        }
        let now = crate::clock::local_now();
        let day = now.date();
        self.days.iter().position(|d| *d == day)?;
        Some((day, i64::from(now.hour()) * 60 + i64::from(now.minute())))
    }

    fn now_line_y(&self, (_day, minute): (Date, i64)) -> f64 {
        let geom = self.geom();
        geom.body_top + minute as f64 / 60.0 * geom.hour_h - self.offset_y()
    }

    fn zoom_by(&mut self, delta: f64) {
        if delta.abs() < f64::EPSILON {
            return;
        }
        let next = (self.zoom * (1.0 + delta * 0.12)).clamp(0.5, 4.0);
        if (next - self.zoom).abs() < 0.001 {
            return;
        }
        self.zoom = next;
        self.layout_dirty = true;
        self.dirty = true;
        self.zoomed.fire(Some(Value::Float(self.zoom)));
    }

    fn geom(&self) -> TimelineGeom {
        geometry(self.rect, &self.tokens(), self.days.len(), self.allday_rows)
    }

    fn offset_y(&self) -> f64 {
        if self.mode == TimelineMode::Month {
            0.0
        } else {
            f64::from(self.scroll.offset_y())
        }
    }

    /// Give the scroll the grid's body and its 24 hours, seeding the hour the person last saw.
    fn layout_scroll(&mut self) {
        if self.mode == TimelineMode::Month {
            return;
        }
        let geom = self.geom();
        self.scroll
            .set_viewport_rect(geom.body(self.rect.x0, self.rect.x1));
        self.scroll.set_content_size(
            crate::num::f32_of_f64(self.rect.width()),
            crate::num::f32_of_f64(geom.content_h),
        );
        if !self.seeded && self.scroll.vertical_overflows() {
            let viewport_h = geom.body_bottom - geom.body_top;
            let max_hours = ((geom.content_h - viewport_h).max(0.0) / geom.hour_h).floor();
            let hour = 8.0_f64.min(max_hours);
            self.scroll
                .set_offset_y(crate::num::f32_of_f64(hour * geom.hour_h));
            self.seeded = true;
        }
    }

    fn column(&self, day: Date) -> Option<usize> {
        self.days.iter().position(|each| *each == day)
    }

    fn item_splits(&self) -> Vec<Split> {
        let mut out = vec![Split::default(); self.items.len()];
        for day in &self.days {
            let indices: Vec<usize> = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.day == *day && !item.all_day)
                .map(|(index, _)| index)
                .collect();
            let intervals: Vec<(i64, i64)> = indices
                .iter()
                .map(|&index| (self.items[index].start_min, self.items[index].end_min))
                .collect();
            let stacks: Vec<i64> = indices
                .iter()
                .map(|&index| self.items[index].stack)
                .collect();
            for (slot, split) in indices.into_iter().zip(splits_for(&intervals, &stacks)) {
                out[slot] = split;
            }
        }
        out
    }

    fn item_rect(&self, index: usize, geom: TimelineGeom, splits: &[Split]) -> Option<Rect> {
        let item = self.items.get(index)?;
        let col = self.column(item.day)?;
        Some(in_split(
            block_rect(
                self.rect,
                self.days.len(),
                geom,
                self.offset_y(),
                col,
                item.start_min,
                item.end_min,
            ),
            splits[index],
        ))
    }

    fn item_at(&self, point: Point) -> Option<(usize, Rect)> {
        if self.mode == TimelineMode::Month {
            return None;
        }
        let geom = self.geom();
        if point.x < self.rect.x0 + geom.gutter || point.x > self.rect.x1 {
            return None;
        }
        let col = column_at(self.rect, self.days.len(), geom, point.x);
        let day = *self.days.get(col)?;
        let splits = self.item_splits();
        self.items
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, item)| item.day == day && !item.all_day)
            .find_map(|(index, _)| {
                let rect = self.item_rect(index, geom, &splits)?;
                rect.contains(point).then_some((index, rect))
            })
    }

    fn on_resize_grip(&self, rect: Rect, point: Point) -> bool {
        let grip = f64::from(crate::tokens::Layout::BASE.resize_grip);
        rect.height() >= 3.0 * grip && point.y >= rect.y1 - grip && point.y <= rect.y1
    }

    fn month_day_at(&self, point: Point) -> Option<Date> {
        if !self.rect.contains(point) {
            return None;
        }
        let (head, cell_w, cell_h) = self.month_metrics();
        let grid_top = self.rect.y0 + head;
        if cell_w <= 0.0 || cell_h <= 0.0 || point.y < grid_top {
            return None;
        }
        let col = ((point.x - self.rect.x0) / cell_w).floor().clamp(0.0, 6.0);
        let row = ((point.y - grid_top) / cell_h).floor().clamp(0.0, 5.0);
        let index = (row * 7.0 + col) as usize;
        self.days.get(index).copied()
    }

    fn day_at(&self, point: Point) -> Option<Date> {
        let geom = self.geom();
        let col = column_at(self.rect, self.days.len(), geom, point.x);
        self.days.get(col).copied()
    }

    fn minute_at(&self, y: f64) -> i64 {
        minute_at(self.geom(), self.offset_y(), y)
    }

    fn begin(&mut self, point: Point) -> bool {
        if self.mode == TimelineMode::Month {
            return false;
        }
        let geom = self.geom();
        if point.x < self.rect.x0 + geom.gutter || !self.rect.contains(point) {
            return false;
        }
        if let Some((index, rect)) = self.item_at(point) {
            if self.on_resize_grip(rect, point) {
                self.drag = Some(Drag {
                    kind: DragKind::Resize { index },
                    origin: point,
                    current: point,
                    active: false,
                });
                return true;
            }
            let grab_off = (self.minute_at(point.y) - self.items[index].start_min).clamp(
                0,
                (self.items[index].end_min - self.items[index].start_min).max(1),
            );
            self.drag = Some(Drag {
                kind: DragKind::Move { index, grab_off },
                origin: point,
                current: point,
                active: false,
            });
            return true;
        }
        let Some(day) = self.day_at(point) else {
            return false;
        };
        self.drag = Some(Drag {
            kind: DragKind::Create {
                day,
                origin: point,
                body: point.y >= geom.body_top,
            },
            current: point,
            origin: point,
            active: false,
        });
        true
    }

    fn update_drag(&mut self, point: Point) {
        if let Some(drag) = &mut self.drag {
            drag.current = point;
            if !drag.active && drag.origin.distance(point) >= DRAG_THRESHOLD {
                drag.active = true;
            }
            self.dirty = true;
        }
    }

    fn effect_for(&self, drag: &Drag) -> Option<Effect> {
        let step = self.slot.max(1);
        match drag.kind {
            DragKind::Create { day, origin, body } => {
                if !body {
                    return None;
                }
                let a = snap_minute(self.minute_at(origin.y), step);
                let b = snap_minute(self.minute_at(drag.current.y), step);
                let (start_min, mut end_min) = (a.min(b), a.max(b));
                if end_min - start_min < step {
                    end_min = (start_min + step).min(MINUTES_PER_DAY);
                }
                Some(Effect::Create {
                    day,
                    start_min,
                    end_min,
                })
            }
            DragKind::Move { index, grab_off } => {
                let geom = self.geom();
                let col = column_at(self.rect, self.days.len(), geom, drag.current.x);
                let day = *self.days.get(col)?;
                let dur = (self.items[index].end_min - self.items[index].start_min).max(step);
                let start_min = snap_minute(self.minute_at(drag.current.y) - grab_off, step)
                    .clamp(0, (MINUTES_PER_DAY - dur).max(0));
                Some(Effect::Move {
                    index,
                    day,
                    start_min,
                })
            }
            DragKind::Resize { index } => {
                let end_min = snap_minute(self.minute_at(drag.current.y), step)
                    .clamp(self.items[index].start_min + step, MINUTES_PER_DAY);
                Some(Effect::Resize { index, end_min })
            }
        }
    }

    fn ghost(&self) -> Option<Ghost> {
        let drag = self.drag.as_ref().filter(|drag| drag.active)?;
        let effect = self.effect_for(drag)?;
        let theme = self.theme.as_ref()?;
        let colors = &theme.tokens.colors;
        let (column, start_min, end_min, title, fill, edge) = match effect {
            Effect::Create {
                day,
                start_min,
                end_min,
            } => (
                self.column(day)?,
                start_min,
                end_min,
                String::new(),
                colors.intent.selection_tint,
                colors.intent.you,
            ),
            Effect::Move {
                index,
                day,
                start_min,
            } => {
                let item = &self.items[index];
                (
                    self.column(day)?,
                    start_min,
                    start_min + (item.end_min - item.start_min),
                    item.title.clone(),
                    colors
                        .tinted_chip_on(
                            colors.surfaces.content,
                            item.color,
                            0.4,
                            crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST,
                        )
                        .fill,
                    item.color,
                )
            }
            Effect::Resize { index, end_min } => {
                let item = &self.items[index];
                (
                    self.column(item.day)?,
                    item.start_min,
                    end_min,
                    item.title.clone(),
                    colors
                        .tinted_chip_on(
                            colors.surfaces.content,
                            item.color,
                            0.4,
                            crate::tokens::colors::SMALL_TEXT_MIN_CONTRAST,
                        )
                        .fill,
                    item.color,
                )
            }
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

    fn finish(&mut self, point: Point) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        self.dirty = true;
        if drag.active {
            if let Some(effect) = self.effect_for(&drag) {
                self.fire_effect(effect);
            }
            return;
        }
        if self.mode != TimelineMode::Month
            && let Some((index, _)) = self.item_at(point)
        {
            self.focus = Some(index);
            self.fire_item(index, "press");
            return;
        }
        if let Some(day) = self.day_at(point) {
            self.open_day.fire(Some(Value::Text(day.to_string())));
        }
    }

    fn fire_item(&self, index: usize, which: &str) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        let Some(events) = self.item_events.get(index) else {
            return;
        };
        let event = match which {
            "move" => &events.moved,
            "resize" => &events.resized,
            _ => &events.press,
        };
        if let Some(resource) = item.resource.clone() {
            event.fire(Some(Value::Resource(resource)));
        }
    }

    fn fire_effect(&self, effect: Effect) {
        match effect {
            Effect::Create {
                day,
                start_min,
                end_min,
            } => {
                let start = datetime_iso(day, start_min);
                let end = datetime_iso(day, end_min);
                self.created.fire(Some(Value::List(vec![
                    Value::Text(start),
                    Value::Text(end),
                ])));
            }
            Effect::Move {
                index,
                day,
                start_min,
            } => {
                if let Some(resource) = self.items[index].resource.clone()
                    && let Some(events) = self.item_events.get(index)
                {
                    events.moved.fire(Some(Value::List(vec![
                        Value::Resource(resource),
                        Value::Text(datetime_iso(day, start_min)),
                    ])));
                }
            }
            Effect::Resize { index, end_min } => {
                let item = &self.items[index];
                if let Some(resource) = item.resource.clone()
                    && let Some(events) = self.item_events.get(index)
                {
                    events.resized.fire(Some(Value::List(vec![
                        Value::Resource(resource),
                        Value::Text(datetime_iso(item.day, end_min)),
                    ])));
                }
            }
        }
    }

    fn move_focus(&mut self, forward: bool) {
        if self.items.is_empty() {
            return;
        }
        let next = match (self.focus, forward) {
            (None, _) => 0,
            (Some(at), true) => (at + 1).min(self.items.len() - 1),
            (Some(at), false) => at.saturating_sub(1),
        };
        self.focus = Some(next);
        self.dirty = true;
    }

    fn month_height(&self, tokens: &TimelineTokens, width: f64) -> f64 {
        let row = (width.max(320.0) / 7.0 * 0.75).max(64.0);
        let head = (tokens.weekday_h + 2.0 * tokens.s2).ceil();
        head + 6.0 * row
    }

    /// How the month grid divides its box: the weekday head's height and a cell's size.
    fn month_metrics(&self) -> (f64, f64, f64) {
        let tokens = self.tokens();
        let head = (tokens.weekday_h + 2.0 * tokens.s2).ceil();
        (
            head,
            self.rect.width() / 7.0,
            (self.rect.height() - head) / 6.0,
        )
    }

    /// Where the month grid's cell `row`, `col` stands, or `None` for a grid with no room in it.
    fn month_cell_rect(&self, row: usize, col: usize) -> Option<Rect> {
        let (head, cell_w, cell_h) = self.month_metrics();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }
        let x0 = self.rect.x0 + cell_w * col as f64;
        let y0 = self.rect.y0 + head + cell_h * row as f64;
        Some(Rect::new(x0, y0, x0 + cell_w, y0 + cell_h))
    }

    /// Where a month cell's `slot`th event chip stands.
    fn month_chip_rect(&self, cell: Rect, slot: usize) -> Rect {
        let tokens = self.tokens();
        let s1 = tokens.s1;
        let pill_r = f64::midpoint(tokens.day_h, s1).ceil();
        let top = cell.y0 + s1 + 2.0 * pill_r + s1 + slot as f64 * (tokens.strip_h + s1);
        Rect::new(cell.x0 + s1, top, cell.x1 - s1, top + tokens.strip_h + s1)
    }

    /// Where an all-day event's chip stands in its day's strip, at `row`.
    fn all_day_rect(&self, column: Rect, row: usize, geom: TimelineGeom) -> Rect {
        let tokens = self.tokens();
        let s1 = tokens.s1;
        let chip_h = if geom.allday_rows <= 1 {
            (geom.allday_h - 2.0 * s1).max(1.0)
        } else {
            tokens.strip_h
        };
        let top = self.rect.y0 + geom.head_h + s1 + row as f64 * (chip_h + s1);
        Rect::new(
            column.x0 + geom.block_inset,
            top,
            column.x1 - geom.block_inset,
            top + chip_h,
        )
    }

    /// The key the day `day` is known by among its days: what an action on one of its events
    /// comes back with.
    fn day_key(day: Date) -> u64 {
        crate::a11y::hash_key(&("timeline.day", day.to_string()))
    }

    /// The key an event is known by among its day's events.
    fn item_key(item: &TimelineItem) -> u64 {
        crate::a11y::hash_key(&("timeline.item", &item.id))
    }

    /// The one thing an action on `item`'s element acts on: the item, by its day and its own key.
    fn item_target(item: &TimelineItem) -> String {
        format!(
            "tilo.key-action:timeline:{}/{}",
            Self::day_key(item.day),
            Self::item_key(item)
        )
    }

    /// When an event is, as a person reads it: its hours, or the days it takes.
    fn item_time_text(item: &TimelineItem) -> String {
        if item.all_day {
            if item.spans_days() {
                crate::format::day_range(item.day, item.end_day)
            } else {
                crate::format::date(item.day)
            }
        } else {
            crate::format::range(
                &super::minute_text(item.start_min),
                &super::minute_text(item.clock_end_min.min(MINUTES_PER_DAY)),
            )
        }
    }

    /// When an event is, as a machine reads it: ISO start and end, or the days it takes.
    fn item_value(item: &TimelineItem) -> String {
        let (start, end) = if item.all_day {
            (item.day.to_string(), item.end_day.to_string())
        } else {
            (
                datetime_iso(item.day, item.start_min),
                datetime_iso(item.day, item.clock_end_min.min(MINUTES_PER_DAY)),
            )
        };
        crate::format::machine!(
            "the span of time an event covers, as a page's <time> holds it",
            if start == end {
                start
            } else {
                format!("{start}/{end}")
            }
        )
    }

    /// The event at `index` as a reader and a page find it: its title, when it is, and the action
    /// a press on it runs.
    fn item_access(&self, index: usize, bounds: Option<Rect>) -> Option<AccessNode> {
        let item = self.items.get(index)?;
        let node = AccessNode::new(Role::Button)
            .with_name(item.title.clone())
            .with_description(Self::item_time_text(item))
            .with_value(Self::item_value(item))
            .with_item(item.id.clone())
            .with_action_target(Self::item_target(item))
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .selected(self.focus == Some(index)),
            );
        Some(match bounds {
            Some(rect) => node.with_bounds(rect),
            None => node,
        })
    }

    /// The day `day` as a reader and a page find it: a group named by its date, its date in a
    /// `<time>`, and what stands on it.
    fn day_access(day: Date, bounds: Rect, items: Vec<AccessNode>) -> AccessNode {
        AccessNode::new(Role::Group)
            .with_name(crate::format::day_heading(day, false))
            .with_value(crate::format::machine!(
                "a day on a timeline, as a page's <time> holds it",
                day.to_string()
            ))
            .with_key(Self::day_key(day))
            .with_bounds(bounds)
            .with_children(items)
    }

    #[cfg(test)]
    pub(crate) fn painted(&self) -> usize {
        self.last_painted
    }

    #[cfg(test)]
    pub(crate) fn layout_for(&mut self, area: Rect, theme: &Theme) {
        self.theme = Some(theme.clone());
        let tokens =
            crate::text_render::with_layout_renderer(theme, |text| TimelineTokens::of(theme, text));
        self.tokens = Some(tokens);
        self.rect = area;
        self.drag = None;
        self.layout_scroll();
    }

    #[cfg(test)]
    pub(crate) fn set_offset_for_test(&mut self, offset: f32) {
        self.scroll.set_offset_y(offset);
        self.seeded = true;
    }

    #[cfg(test)]
    pub(crate) fn begin_for_test(&mut self, point: Point) -> bool {
        self.begin(point)
    }

    #[cfg(test)]
    pub(crate) fn finish_for_test(&mut self, point: Point) {
        self.finish(point);
    }

    #[cfg(test)]
    pub(crate) fn update_for_test(&mut self, point: Point) {
        self.update_drag(point);
    }

    #[cfg(test)]
    pub(crate) fn set_items_for_test(&mut self, items: Vec<TimelineItem>) {
        self.items = items;
        self.item_events = vec![ItemEvents::default(); self.items.len()];
        self.allday_rows = super::all_day_rows(&self.items).max(self.min_rows);
    }

    #[cfg(test)]
    pub(crate) fn rect_for_test(&self, index: usize) -> Option<Rect> {
        let geom = self.geom();
        let splits = self.item_splits();
        self.item_rect(index, geom, &splits)
    }

    #[cfg(test)]
    pub(crate) fn set_events_for_test(&mut self, events: (Event, Event, Event)) {
        (self.created, self.open_day, self.zoomed) = events;
    }

    #[cfg(test)]
    pub(crate) fn set_item_events_for_test(&mut self, events: Vec<ItemEvents>) {
        self.item_events = events;
    }
}

fn period_days(mode: TimelineMode, anchor: Date) -> Vec<Date> {
    match mode {
        TimelineMode::Day => vec![anchor],
        TimelineMode::Week => {
            let first = crate::format::first_weekday();
            let into_week = i64::from(anchor.weekday().since(first));
            let start = anchor - jiff::Span::new().days(into_week);
            (0..7).map(|i| start + jiff::Span::new().days(i)).collect()
        }
        TimelineMode::Month => super::month_grid(anchor),
    }
}

fn datetime_iso(day: Date, minute: i64) -> String {
    let (day, minute) = if minute >= MINUTES_PER_DAY {
        (day + jiff::Span::new().days(1), 0)
    } else {
        (day, minute.max(0))
    };
    #[allow(clippy::cast_possible_truncation, reason = "a minute of a day fits i8")]
    let (hour, min) = ((minute / 60) as i8, (minute % 60) as i8);
    DateTime::new(day.year(), day.month(), day.day(), hour, min, 0, 0)
        .map_or_else(|_| day.to_string(), |at| at.to_string())
}

impl Widget for Timeline {
    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(theme) = self.theme.clone() else {
            return taffy::Size::ZERO;
        };
        let mut tokens = TimelineTokens::of(&theme, text_renderer);
        tokens.zoom = self.zoom;
        self.tokens = Some(tokens.clone());
        self.start_slide(&theme);
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        let height = if self.mode == TimelineMode::Month {
            self.month_height(&tokens, f64::from(width))
        } else {
            let natural =
                natural_height(&tokens, f64::from(width), self.days.len(), self.allday_rows);
            // A slot with less room than 24 legible hours keeps what it is given and scrolls the
            // rest; a taller one shows the whole day.
            match available.height {
                taffy::AvailableSpace::Definite(offered)
                    if offered.is_finite() && f64::from(offered) < natural =>
                {
                    f64::from(offered)
                }
                _ => natural,
            }
        };
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(height),
        }
    }

    fn apply_layout(&mut self, taffy: &taffy::TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.layout_scroll();
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO || self.layout_dirty
    }

    fn layout_measured(&mut self) {
        self.layout_dirty = false;
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if env.should_cull(self.rect) || self.rect.width() <= 0.0 {
            return;
        }
        self.theme = Some(theme.clone());
        self.start_slide(theme);
        self.resolve_colors(theme);
        let mut tokens = TimelineTokens::of(theme, text_renderer);
        tokens.zoom = self.zoom;
        self.tokens = Some(tokens.clone());
        let now = crate::motion::frame_now();
        let slide = self.slide.get(now) * self.rect.width();
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        let moved = transform * Affine::translate((slide, 0.0));
        let mut ctx = ctx.with_transform(moved);
        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        match self.mode {
            TimelineMode::Month => {
                let face = MonthFace {
                    anchor: self.anchor,
                    today: self.today,
                    items: &self.items,
                    untitled: "",
                };
                let report = face.paint(&mut ctx, self.rect, None);
                self.last_painted = report.painted;
            }
            TimelineMode::Day | TimelineMode::Week => {
                let ghost = self.ghost();
                let moving = self
                    .drag
                    .as_ref()
                    .filter(|drag| drag.active)
                    .and_then(|drag| match drag.kind {
                        DragKind::Move { index, .. } | DragKind::Resize { index } => {
                            self.items.get(index).map(|item| item.id.clone())
                        }
                        DragKind::Create { .. } => None,
                    });
                let now = self.now_here();
                self.now_y = now.map(|each| self.now_line_y(each));
                let face = TimelineFace {
                    days: &self.days,
                    tokens: &tokens,
                    today: self.today,
                    now,
                    items: &self.items,
                    untitled: "",
                    allday_rows: self.allday_rows,
                    ghost: ghost.as_ref(),
                    moving,
                };
                let report = face.paint(&mut ctx, self.rect, self.offset_y());
                self.last_painted = report.painted;
            }
        }
        ctx.scene.pop_layer();
        if self.mode != TimelineMode::Month {
            self.scroll.draw_bar_overlay(ctx.scene, transform, theme);
        }
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.mode != TimelineMode::Month
            && ctx.modifiers.state().control_key()
            && let WindowEvent::MouseWheel { delta, .. } = event
        {
            let step = match delta {
                MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                MouseScrollDelta::PixelDelta(position) => position.y / 48.0,
            };
            self.zoom_by(step);
            *ctx.handled = true;
            return;
        }
        if self.mode != TimelineMode::Month {
            self.scroll.handle_event(event, ctx);
            if *ctx.handled {
                return;
            }
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = ctx.logical(position);
                if self.drag.is_some() {
                    self.update_drag(self.pointer);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.begin(self.pointer) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.mode == TimelineMode::Month {
                    if let Some(day) = self.month_day_at(self.pointer) {
                        self.open_day.fire(Some(Value::Text(day.to_string())));
                        *ctx.handled = true;
                    }
                } else if self.drag.is_some() {
                    self.finish(self.pointer);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.focus.is_some() =>
            {
                match event.logical_key {
                    Key::Named(NamedKey::ArrowDown | NamedKey::ArrowRight) => {
                        self.move_focus(true);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp | NamedKey::ArrowLeft) => {
                        self.move_focus(false);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(index) = self.focus {
                            self.fire_item(index, "press");
                        }
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Escape) => {
                        self.drag = None;
                        self.dirty = true;
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag.as_ref().is_some_and(|drag| drag.active) || self.scroll.wants_pointer_capture()
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if focused && self.focus.is_none() && !self.items.is_empty() {
            self.focus = Some(0);
        } else if !focused {
            self.focus = None;
        }
        self.dirty = true;
    }

    fn is_focused(&self) -> bool {
        self.focus.is_some()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.scroll.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.scroll.mark_drawn();
    }

    fn needs_animation_frame(&self) -> bool {
        self.slide.is_moving(crate::motion::frame_now()) || self.scroll.needs_animation_frame()
    }

    /// Wake at the next whole minute while the now line is shown, so it is repainted when it
    /// crosses a pixel row and never every frame.
    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.now_here()?;
        let second = i64::from(crate::clock::local_now().second());
        let wait = u64::try_from((60 - second).clamp(1, 60)).unwrap_or(60);
        Some(crate::time::now() + crate::time::Duration::from_secs(wait))
    }

    fn serve_deadline(&mut self, _now: crate::time::Instant) -> bool {
        let Some(now) = self.now_here() else {
            return false;
        };
        let y = self.now_line_y(now);
        if self.now_y.is_some_and(|drawn| (drawn - y).abs() < 0.5) {
            return false;
        }
        self.now_y = Some(y);
        self.dirty = true;
        true
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.mode == TimelineMode::Month {
            return Some(winit::window::CursorIcon::Default);
        }
        if let Some((_, rect)) = self.item_at(point) {
            if self.on_resize_grip(rect, point) {
                return Some(winit::window::CursorIcon::NsResize);
            }
            return Some(winit::window::CursorIcon::Pointer);
        }
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::Crosshair)
    }

    fn name(&self) -> &'static str {
        "Timeline"
    }

    /// The grid as a reader hears it: each day a group named by its date, and what stands on it.
    /// Only what the viewport shows is a node, as only what it shows is painted, but every
    /// all-day event is one wherever its day is shown.
    fn accessibility(&self) -> Option<AccessNode> {
        // Each event once, under the day it starts on, so neither the week's few days nor the
        // month's many read the whole list again.
        let mut on_day: Vec<Vec<(usize, &TimelineItem)>> = vec![Vec::new(); self.days.len()];
        for (index, item) in self.items.iter().enumerate() {
            if let Some(col) = self.column(item.day)
                && let Some(day) = on_day.get_mut(col)
            {
                day.push((index, item));
            }
        }
        let geom = self.geom();
        let splits = if self.mode == TimelineMode::Month {
            Vec::new()
        } else {
            self.item_splits()
        };
        let mut days = Vec::new();
        for (col, day) in self.days.iter().enumerate() {
            let Some(on_day) = on_day.get(col) else {
                continue;
            };
            if self.mode == TimelineMode::Month {
                let Some(cell) = self.month_cell_rect(col / 7, col % 7) else {
                    continue;
                };
                let mut items = Vec::new();
                let mut on_cell: Vec<(usize, &TimelineItem)> = on_day.clone();
                on_cell.sort_by_key(|(_, item)| item.start_min);
                for (slot, (at, _)) in on_cell.into_iter().enumerate() {
                    let rect = self.month_chip_rect(cell, slot);
                    // A cell shows the chips it has room for; the rest are its "more" row.
                    if rect.y0 >= cell.y1 {
                        continue;
                    }
                    if let Some(node) = self.item_access(at, Some(rect)) {
                        items.push(node);
                    }
                }
                days.push(Self::day_access(*day, cell, items));
            } else {
                let column_w = super::column_width(self.rect, self.days.len(), geom);
                let x0 = self.rect.x0 + geom.gutter + column_w * col as f64;
                let column = Rect::new(x0, self.rect.y0, x0 + column_w, self.rect.y1);
                let mut items = Vec::new();
                let mut all_day: Vec<(usize, &TimelineItem)> = on_day
                    .iter()
                    .copied()
                    .filter(|(_, item)| item.all_day)
                    .collect();
                all_day.sort_by_key(|(_, item)| std::cmp::Reverse(item.nights()));
                for (row, (index, _)) in all_day.into_iter().enumerate() {
                    let rect = self.all_day_rect(column, row, geom);
                    if let Some(node) = self.item_access(index, Some(rect)) {
                        items.push(node);
                    }
                }
                let mut timed: Vec<(usize, &TimelineItem)> = on_day
                    .iter()
                    .copied()
                    .filter(|(_, item)| !item.all_day)
                    .collect();
                timed.sort_by_key(|(_, item)| (item.start_min, item.id.as_str()));
                for (index, _) in timed {
                    let Some(rect) = self.item_rect(index, geom, &splits) else {
                        continue;
                    };
                    if rect.intersect(self.rect).height() <= 0.0 {
                        continue;
                    }
                    if let Some(node) = self.item_access(index, Some(rect)) {
                        items.push(node);
                    }
                }
                days.push(Self::day_access(*day, column, items));
            }
        }
        Some(
            AccessNode::new(Role::Group)
                .with_optional_name(self.label.clone())
                .with_state(AccessState::new().focused(self.focus.is_some()))
                .with_children(days),
        )
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        if !matches!(action, Action::Activate | Action::Focus) {
            return false;
        }
        // The element an action names carries the event's own key: the last of its path. One an
        // assistive technology held across a change may name an event that is gone, which is
        // nothing to press rather than the event now at its old place.
        let index = match key_path.last() {
            Some(&key) => match self
                .items
                .iter()
                .position(|item| Self::item_key(item) == key)
            {
                Some(index) => index,
                None => return false,
            },
            None => match self.focus {
                Some(index) if self.items.get(index).is_some() => index,
                _ => return false,
            },
        };
        self.focus = Some(index);
        self.dirty = true;
        if matches!(action, Action::Activate) {
            self.fire_item(index, "press");
        }
        true
    }

    #[cfg(test)]
    fn test_rects(&self) -> Vec<(String, Rect)> {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                self.rect_for_test(index)
                    .map(|rect| (item.id.clone(), rect))
            })
            .collect()
    }
}
