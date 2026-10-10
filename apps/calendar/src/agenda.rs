//! Agenda list component.
//!
//! This owns the agenda view's scroll state, geometry, input routing, drawing,
//! and semantic row actions. `CalendarApp` supplies the model and translates
//! those actions into its document mutations/navigation. Each day is a group
//! under its heading; each event and task is a shared list row.

use crate::input_context::InputContext;
use std::collections::HashSet;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use chrono::{Duration, NaiveDate};
use vello::kurbo::{Point, Rect};
use vello::peniko::Fill;

use ui::Widget;
use ui::icons::Icon;
use ui::tokens::TextRole;
use ui::widget::empty_state::{EmptyState, EmptyStateContent};
use ui::widget::{Interaction, ListRowFace, RowLeading, Scrollable};

use crate::date;
use crate::draw_util::text_in_band;
use crate::model::{Calendar, Event, Id, Priority, Task};

const DAYS_SHOWN: i64 = 21;

/// The last day the agenda lists when it starts on `anchor`.
pub(crate) fn last_day(anchor: NaiveDate) -> NaiveDate {
    anchor + Duration::days(DAYS_SHOWN - 1)
}

/// The empty agenda: one sentence. Its action is the header's New event,
/// which is already on screen.
const EMPTY_TITLE: &str = "Nothing scheduled in the next three weeks";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgendaAction {
    OpenEvent(Id),
    OpenTask(Id),
    ToggleTask(Id),
}

/// One row of the agenda, in list order.
enum Item<'a> {
    Heading(NaiveDate),
    Event(&'a Event),
    Task(&'a Task),
}

/// The agenda's geometry in a theme: the list's insets and the gaps
/// between its groups.
struct Metrics {
    gutter: f64,
    heading_gap: f64,
    group_gap: f64,
}

impl Metrics {
    fn of(theme: &Theme) -> Self {
        let rhythm = &theme.tokens.spacing.rhythm;
        Self {
            gutter: f64::from(theme.tokens.spacing.s6),
            heading_gap: f64::from(rhythm.related),
            group_gap: f64::from(rhythm.group),
        }
    }
}

pub(crate) struct AgendaView {
    scroll: Scrollable,
}

impl AgendaView {
    pub(crate) fn new() -> Self {
        Self {
            scroll: Scrollable::new_vertical().always_visible(),
        }
    }

    pub(crate) fn layout(&mut self, area: Rect, source: &AgendaSource<'_>, theme: &Theme) {
        let height = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
            content_height(source, text, theme)
        });
        self.scroll.set_viewport_rect(area);
        self.scroll
            .set_content_size(area.width() as f32, height as f32);
    }

    pub(crate) fn handle_event(&mut self, input: &mut InputContext<'_>) -> bool {
        self.scroll.handle_event(
            input.event,
            &mut ui::widget::EventCtx {
                scale_factor: input.scale_factor,
                modifiers: &input.modifiers,
                state: input.state.reborrow(),
                path: input.path,
                handled: input.handled,
                focus_target: input.focus_target,
                node_id: input.node_id,
                taffy: input.taffy,
            },
        );
        *input.handled
    }

    pub(crate) fn wants_pointer_capture(&self) -> bool {
        self.scroll.wants_pointer_capture()
    }

    pub(crate) fn draw(
        &self,
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        source: &AgendaSource<'_>,
        today: NaiveDate,
        pointer: Point,
    ) -> Vec<(Rect, AgendaAction)> {
        let theme = ctx.theme;
        let m = Metrics::of(theme);
        let items = source.items();
        let mut hits = Vec::new();
        if items.is_empty() {
            let content = EmptyStateContent {
                icon: Some(Icon::Calendar),
                ..EmptyStateContent::new(EMPTY_TITLE)
            };
            EmptyState::paint(ctx, area, &content, pointer);
            return hits;
        }

        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &area,
        );
        let (x0, x1) = (area.x0 + m.gutter, area.x1 - m.gutter);
        let mut y = area.y0 + m.gutter - f64::from(self.scroll.offset_y());
        for (index, item) in items.iter().enumerate() {
            let next_is_row = matches!(items.get(index + 1), Some(Item::Event(_) | Item::Task(_)));
            match item {
                Item::Heading(day) => {
                    if index > 0 {
                        y += m.group_gap;
                    }
                    let h = heading_height(ctx.text, theme);
                    let band = Rect::new(x0, y, x1, y + h);
                    if band.y1 > area.y0 && band.y0 < area.y1 {
                        draw_heading(ctx, band, *day, today);
                    }
                    y = band.y1 + m.heading_gap;
                }
                Item::Event(event) => {
                    let time = event_time(event);
                    let face = event_face(event, &time, theme);
                    let h = face.height(ctx.text, theme);
                    let row = Rect::new(x0, y, x1, y + h);
                    if row.y1 > area.y0 && row.y0 < area.y1 {
                        let face = face.interaction(Interaction {
                            hovered: row.contains(pointer),
                            ..Interaction::default()
                        });
                        let face = if next_is_row {
                            face.separator(false)
                        } else {
                            face
                        };
                        face.paint(ctx, row);
                        if let Some(hit) = clamp_hit(row, area) {
                            hits.push((hit, AgendaAction::OpenEvent(event.id)));
                        }
                    }
                    y = row.y1;
                }
                Item::Task(task) => {
                    let face = task_face(task, source, theme);
                    let h = face.height(ctx.text, theme);
                    let row = Rect::new(x0, y, x1, y + h);
                    if row.y1 > area.y0 && row.y0 < area.y1 {
                        let face = if next_is_row {
                            face.separator(false)
                        } else {
                            face
                        };
                        paint_task_row(ctx, face, row, (area, pointer), task, &mut hits);
                    }
                    y = row.y1;
                }
            }
        }
        ctx.scene.pop_layer();
        self.scroll
            .draw_bar_overlay(ctx.scene, ctx.transform, theme);
        hits
    }

    pub(crate) fn needs_redraw(&self) -> bool {
        self.scroll.needs_redraw()
    }

    pub(crate) fn mark_drawn(&mut self) {
        self.scroll.mark_drawn();
    }

    pub(crate) fn needs_animation_frame(&self) -> bool {
        self.scroll.needs_animation_frame()
    }

    pub(crate) fn is_layout_dirty(&self) -> bool {
        self.scroll.is_layout_dirty()
    }
}

/// Paint one task row in `row`, clipped to `area`, and push its targets:
/// the row opens the task; the checkbox, pushed after it so it wins,
/// completes it.
fn paint_task_row(
    ctx: &mut PaintCtx<'_>,
    face: ListRowFace<'_>,
    row: Rect,
    (area, pointer): (Rect, Point),
    task: &Task,
    hits: &mut Vec<(Rect, AgendaAction)>,
) {
    let theme = ctx.theme;
    let face = face.interaction(Interaction {
        hovered: row.contains(pointer),
        ..Interaction::default()
    });
    face.paint(ctx, row);
    if let Some(hit) = clamp_hit(row, area) {
        hits.push((hit, AgendaAction::OpenTask(task.id)));
    }
    let lead = face.leading_rect(row, ctx.text, theme);
    if let Some(hit) = clamp_hit(lead, area) {
        hits.push((hit, AgendaAction::ToggleTask(task.id)));
    }
}

/// An event's time as its row's meta: its start, or "All day".
fn event_time(event: &Event) -> String {
    if event.all_day {
        "All day".into()
    } else {
        date::fmt_time(&event.start)
    }
}

/// An event row: its calendar colour leads, its time is the meta.
fn event_face<'a>(event: &'a Event, time: &'a str, theme: &Theme) -> ListRowFace<'a> {
    let face = ListRowFace::new(event.display_title())
        .leading(RowLeading::Swatch(event.color.color(theme)))
        .meta(time);
    if event.location.is_empty() {
        face
    } else {
        face.secondary(&event.location)
    }
}

/// A task row: its checkbox leads and completes it; its project's colour
/// follows the title as a swatch mark.
fn task_face<'a>(task: &'a Task, source: &AgendaSource<'_>, theme: &Theme) -> ListRowFace<'a> {
    let mut face =
        ListRowFace::new(&task.title).leading(RowLeading::Checkbox { checked: task.done });
    if let Some(project) = task.project.and_then(|id| source.model.project(id)) {
        face = face.swatch_mark(project.color.color(theme));
    }
    if task.done {
        face.meta("Done")
    } else if task.priority == Priority::None {
        face
    } else {
        face.meta(task.priority.label())
    }
}

/// A day heading: "Today", "Tomorrow" or "Yesterday" with the date beside
/// it in quieter ink, or just the date further out.
fn draw_heading(ctx: &mut PaintCtx<'_>, band: Rect, day: NaiveDate, today: NaiveDate) {
    let theme = ctx.theme;
    let long = date::fmt_day_long(day);
    let near = (day - today).num_days().abs() <= 1;
    let label = if near {
        date::relative_label(day, today)
    } else {
        long.clone()
    };
    let heading = theme.text(TextRole::Subheading);
    text_in_band(
        ctx,
        &label,
        &heading,
        theme.text_primary,
        band.x0,
        band,
        band.width(),
    );
    if near {
        let width = f64::from(ctx.text.measure_style(&label, None, &heading).0);
        let x = band.x0 + width + f64::from(theme.tokens.spacing.s2);
        text_in_band(
            ctx,
            &long,
            &theme.text(TextRole::Caption),
            theme.text_dim,
            x,
            band,
            band.x1 - x,
        );
    }
}

fn heading_height(text: &mut TextRenderer, theme: &Theme) -> f64 {
    f64::from(
        text.measure_style("Ag", None, &theme.text(TextRole::Subheading))
            .1,
    )
}

/// What the agenda lists: the model's events and top-level tasks from
/// `anchor` onward, with hidden calendars and the calendar filter applied.
pub(crate) struct AgendaSource<'a> {
    pub(crate) model: &'a Calendar,
    pub(crate) anchor: NaiveDate,
    pub(crate) hidden_calendars: &'a HashSet<String>,
    pub(crate) calendar_filter: Option<&'a str>,
}

impl<'a> AgendaSource<'a> {
    fn events_on(&self, day: NaiveDate) -> Vec<&'a Event> {
        visible_events_on(self.model, day, self.hidden_calendars, self.calendar_filter)
    }

    fn tasks_on(&self, day: NaiveDate) -> Vec<&'a Task> {
        let mut tasks: Vec<&'a Task> = self
            .model
            .tasks
            .iter()
            .filter(|task| task.due == Some(day) && task.parent.is_none())
            .collect();
        tasks.sort_by(|a, b| Task::list_order(a, b));
        tasks
    }

    /// Every heading and row the agenda shows, in order.
    fn items(&self) -> Vec<Item<'a>> {
        let end = last_day(self.anchor);
        let mut items = Vec::new();
        let mut day = self.anchor;
        while day <= end {
            let events = self.events_on(day);
            let tasks = self.tasks_on(day);
            if !events.is_empty() || !tasks.is_empty() {
                items.push(Item::Heading(day));
                items.extend(events.into_iter().map(Item::Event));
                items.extend(tasks.into_iter().map(Item::Task));
            }
            day += Duration::days(1);
        }
        items
    }
}

fn visible_events_on<'a>(
    model: &'a Calendar,
    date: NaiveDate,
    hidden_calendars: &HashSet<String>,
    calendar_filter: Option<&str>,
) -> Vec<&'a Event> {
    model
        .events_on(date)
        .into_iter()
        .filter(|event| {
            event.visible_under(hidden_calendars)
                && calendar_filter.is_none_or(|calendar| {
                    if calendar == "local" {
                        event.calendar_id.is_none()
                    } else {
                        event.calendar_id.as_deref() == Some(calendar)
                    }
                })
        })
        .collect()
}

/// The list's full height: what [`AgendaView::draw`] paints, top to bottom.
fn content_height(source: &AgendaSource<'_>, text: &mut TextRenderer, theme: &Theme) -> f64 {
    let m = Metrics::of(theme);
    let items = source.items();
    if items.is_empty() {
        return 0.0;
    }
    let mut height = 2.0 * m.gutter;
    for (index, item) in items.iter().enumerate() {
        height += match item {
            Item::Heading(_) => {
                heading_height(text, theme)
                    + m.heading_gap
                    + if index > 0 { m.group_gap } else { 0.0 }
            }
            Item::Event(event) => {
                let time = event_time(event);
                event_face(event, &time, theme).height(text, theme)
            }
            Item::Task(task) => task_face(task, source, theme).height(text, theme),
        };
    }
    height
}

fn clamp_hit(row: Rect, viewport: Rect) -> Option<Rect> {
    let hit = row.intersect(viewport);
    (hit.area() > 0.0).then_some(hit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, day).expect("valid fixture date")
    }

    fn source<'a>(
        model: &'a Calendar,
        anchor: NaiveDate,
        hidden_calendars: &'a HashSet<String>,
    ) -> AgendaSource<'a> {
        AgendaSource {
            model,
            anchor,
            hidden_calendars,
            calendar_filter: None,
        }
    }

    fn event(day: u32, calendar_id: Option<&str>) -> Event {
        let start = date(day).and_hms_opt(9, 0, 0).expect("valid fixture time");
        Event {
            id: Uuid::new_v4(),
            title: "Event".into(),
            start,
            end: start + Duration::hours(1),
            calendar_id: calendar_id.map(str::to_owned),
            ..Event::default()
        }
    }

    #[test]
    fn content_extent_and_visible_events_respect_hidden_and_selected_calendars() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        // Case `content_extent_grows_with_headings_and_rows`.
        {
            let anchor = date(1);
            let one = Calendar {
                events: vec![event(1, None)].into(),
                ..Calendar::default()
            };
            let two = Calendar {
                events: vec![event(1, None)].into(),
                tasks: vec![Task {
                    id: Uuid::new_v4(),
                    title: "Task".into(),
                    done: false,
                    due: Some(anchor),
                    priority: Priority::None,
                    project: None,
                    parent: None,
                }]
                .into(),
                ..Calendar::default()
            };
            let hidden = HashSet::new();
            let h1 = content_height(&source(&one, anchor, &hidden), &mut text, &theme);
            let h2 = content_height(&source(&two, anchor, &hidden), &mut text, &theme);
            assert!(h1 > 0.0);
            assert!(h2 > h1, "a second row grows the list");
        }
        // Case `hidden_remote_calendars_do_not_expand_the_scroll_extent`.
        {
            let anchor = date(1);
            let model = Calendar {
                events: vec![event(1, Some("hidden"))].into(),
                ..Calendar::default()
            };
            let hidden = HashSet::from(["hidden".to_string()]);
            assert_eq!(
                content_height(&source(&model, anchor, &hidden), &mut text, &theme),
                0.0
            );
            assert!(
                content_height(&source(&model, anchor, &HashSet::new()), &mut text, &theme) > 0.0
            );
        }
        // Case `selected_calendar_filters_events_without_overriding_visibility`.
        {
            let anchor = date(1);
            let model = Calendar {
                events: vec![
                    event(1, None),
                    event(1, Some("work")),
                    event(1, Some("home")),
                ]
                .into(),
                ..Calendar::default()
            };
            assert_eq!(
                visible_events_on(&model, anchor, &HashSet::new(), Some("work")).len(),
                1
            );
            assert!(
                visible_events_on(
                    &model,
                    anchor,
                    &HashSet::from(["work".to_string()]),
                    Some("work")
                )
                .is_empty()
            );
            assert_eq!(
                visible_events_on(&model, anchor, &HashSet::new(), Some("local")).len(),
                1
            );
        }
    }

    #[test]
    fn a_days_tasks_list_by_priority_whatever_order_the_document_holds() {
        let anchor = date(1);
        let task = |title: &str, priority: Priority| Task {
            id: Uuid::new_v4(),
            title: title.into(),
            done: false,
            due: Some(anchor),
            priority,
            project: None,
            parent: None,
        };
        let tasks = vec![
            task("Drink water", Priority::None),
            task("Send meeting notes", Priority::Medium),
            task("Prepare quarterly report", Priority::High),
            task("Book a table", Priority::None),
        ];
        let hidden = HashSet::new();
        let mut reversed = tasks.clone();
        reversed.reverse();
        for tasks in [tasks, reversed] {
            let model = Calendar {
                tasks: tasks.into(),
                ..Calendar::default()
            };
            let titles: Vec<&str> = source(&model, anchor, &hidden)
                .tasks_on(anchor)
                .iter()
                .map(|task| task.title.as_str())
                .collect();
            assert_eq!(
                titles,
                [
                    "Prepare quarterly report",
                    "Send meeting notes",
                    "Book a table",
                    "Drink water"
                ]
            );
        }
    }

    #[test]
    fn larger_text_grows_the_list() {
        let anchor = date(1);
        let model = Calendar {
            events: vec![event(1, None)].into(),
            ..Calendar::default()
        };
        let hidden = HashSet::new();
        let mut text = TextRenderer::new();
        let one = content_height(&source(&model, anchor, &hidden), &mut text, &Theme::dark());
        let two = content_height(
            &source(&model, anchor, &hidden),
            &mut text,
            &Theme::dark().with_text_scale(2.0),
        );
        assert!(two > one);
    }
}
