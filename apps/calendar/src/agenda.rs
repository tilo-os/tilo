//! Agenda list component.
//!
//! This owns the agenda view's scroll state, geometry, input routing, drawing,
//! and semantic row actions. `CalendarApp` supplies the model and translates
//! those actions into its document mutations/navigation.

use crate::input_context::InputContext;
use std::collections::HashSet;
use ui::paint::PaintCtx;
use ui::text_render::TextRenderer;

use chrono::{Duration, NaiveDate};
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

use ui::Widget;
use ui::tokens::spacing::Spacing;
use ui::widget::Scrollable;

use crate::app::{center_text, draw_checkbox};
use crate::date;
use crate::draw_util::{fill_round, stroke_line};
use crate::metrics;
use crate::model::{Id, Model, Priority};

const DAYS_SHOWN: i64 = 21;

/// Base text sizes of the agenda's three row kinds.
const HEADING_TEXT: f32 = 15.0;
const HEADING_DATE_TEXT: f32 = 12.0;
const ROW_TEXT: f32 = 13.0;
const ROW_META_TEXT: f32 = 12.0;
const ROW_FLAG_TEXT: f32 = 11.0;
/// Authored 1× heights; both grow with the measured line at larger scales.
const HEADING_MIN_H: f64 = 26.0;
const ROW_MIN_H: f64 = 30.0;
/// Gap between consecutive rows. Deliberately off-scale (2px) — the authored
/// 1× row pitch of 32px.
const ROW_GAP: f64 = 2.0;
/// Top padding above the first heading. Deliberately off-scale (18px), the
/// authored 1× offset that lines the first heading up with the topbar title.
const TOP_PAD: f64 = 18.0;
/// Left edge of the day-heading date relative to the heading label.
const HEADING_DATE_X: f64 = 120.0;
/// Share of a row the trailing location column may take before eliding.
const LOCATION_MAX_SHARE: f64 = 0.35;
/// Trailing width of a task row that opens the task rather than toggling
/// it, however long the title. Matches the authored 1× reserve (the label
/// stopped 160px short of the row end; the toggle rect 40px short).
const TASK_OPEN_MIN_W: f64 = 120.0;

/// Row geometry for the current text scale. Built once per layout/draw and
/// consumed by the scroll extent, the painter and the hit rects alike, so the
/// agenda's three geometry consumers cannot disagree.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AgendaMetrics {
    /// Day heading row height.
    pub heading_h: f64,
    /// Line height of the heading label / date.
    pub heading_text_h: f64,
    pub heading_date_h: f64,
    /// Event / task row height and pitch.
    pub row_h: f64,
    pub row_pitch: f64,
    /// Line heights of the row's title, meta (time / location) and flag text.
    pub row_text_h: f64,
    pub row_meta_h: f64,
    pub row_flag_h: f64,
    /// Gap after the last row of a day, before the next heading.
    pub section_gap: f64,
    /// Inner horizontal insets read from the spacing scale.
    pub bar_inset: f64,
    /// Horizontal inset of the whole list inside its area (`s6`), and the
    /// pad after the last row (`s2`). Read from the theme, never restated.
    pub side_pad: f64,
    pub bottom_pad: f64,
    pub time_x: f64,
    pub check_gap: f64,
    /// Title column start for event rows (after the time column).
    pub title_x: f64,
    /// Task checkbox size — one text line, so it grows with the label.
    pub check_size: f64,
}

impl AgendaMetrics {
    pub(crate) fn new(spacing: &Spacing, text_scale: f32) -> Self {
        let heading_text_h = metrics::line_h(HEADING_TEXT, text_scale);
        let heading_date_h = metrics::line_h(HEADING_DATE_TEXT, text_scale);
        let row_text_h = metrics::line_h(ROW_TEXT, text_scale);
        let row_meta_h = metrics::line_h(ROW_META_TEXT, text_scale);
        let row_flag_h = metrics::line_h(ROW_FLAG_TEXT, text_scale);
        // The authored rows are the 1× design exactly; at larger scales they
        // grow by their line's extra height and snap up to a whole pixel, so a
        // scaled row is never a fraction short of its text plus the padding
        // the design gave it.
        let heading_h =
            metrics::at_least_whole(HEADING_MIN_H, heading_text_h + f64::from(spacing.s1));
        let row_h = metrics::at_least_whole(ROW_MIN_H, row_text_h + f64::from(spacing.s3));
        let time_x = f64::from(spacing.s4);
        Self {
            heading_h,
            heading_text_h,
            heading_date_h,
            row_h,
            row_pitch: row_h + ROW_GAP,
            row_text_h,
            row_meta_h,
            row_flag_h,
            section_gap: f64::from(spacing.s3),
            bar_inset: f64::from(spacing.s1),
            side_pad: f64::from(spacing.s6),
            bottom_pad: f64::from(spacing.s2),
            time_x,
            check_gap: f64::from(spacing.s2),
            title_x: time_x + TIME_COL_MIN_W,
            check_size: row_text_h.round(),
        }
    }

    /// Size the time column from the widest time label at the current scale
    /// so a scaled "All day" never runs into the title. Paint-only: the scroll
    /// extent does not depend on it.
    pub(crate) fn measure_time_column(&mut self, tr: &mut TextRenderer, spacing: &Spacing) {
        let widest = f64::from(tr.measure(ALL_DAY_LABEL, ROW_META_TEXT, None).0);
        self.title_x =
            self.time_x + metrics::at_least(TIME_COL_MIN_W, widest + f64::from(spacing.s3));
    }
}

/// Authored 1× width of the time column (title starts at 16 + 74 = 90).
const TIME_COL_MIN_W: f64 = 74.0;
/// The widest time label an event row shows.
const ALL_DAY_LABEL: &str = "All day";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgendaAction {
    OpenEvent(Id),
    OpenTask(Id),
    ToggleTask(Id),
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

    pub(crate) fn layout(
        &mut self,
        area: Rect,
        source: &AgendaSource<'_>,
        metrics: &AgendaMetrics,
    ) {
        self.scroll.set_viewport_rect(area);
        self.scroll
            .set_content_size(area.width() as f32, content_height(source, metrics) as f32);
    }

    pub(crate) fn handle_event(&mut self, input: &mut InputContext<'_>) -> bool {
        self.scroll.handle_event(
            input.event,
            &mut ui::widget::EventCtx {
                scale_factor: input.scale_factor,
                modifiers: &input.modifiers,
                state: input.state,
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
        let colors = &theme.tokens.colors;
        ctx.fill_rect(area, colors.surfaces.base);

        let end = source.anchor + Duration::days(DAYS_SHOWN);
        let mut m = AgendaMetrics::new(&theme.tokens.spacing, ctx.text.text_scale());
        m.measure_time_column(ctx.text, &theme.tokens.spacing);
        let mut rows = RowPainter {
            ctx: ctx.reborrow(),
            area,
            x0: area.x0 + m.side_pad,
            show_location: area.width() >= 520.0,
            pointer,
            m,
            hits: Vec::new(),
        };
        let mut y = area.y0 + TOP_PAD - f64::from(self.scroll.offset_y());
        let mut drew_any = false;

        rows.ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            rows.ctx.transform,
            &area,
        );

        let mut day = source.anchor;
        while day <= end {
            let events = source.events_on(day);
            let tasks = source.tasks_on(day);
            if events.is_empty() && tasks.is_empty() {
                day += Duration::days(1);
                continue;
            }
            drew_any = true;

            rows.draw_day_heading(y, day, today);
            y += m.heading_h;

            for event in events {
                rows.draw_event_row(y, event);
                y += m.row_pitch;
            }

            for task in tasks {
                rows.draw_task_row(y, task);
                y += m.row_pitch;
            }
            y += m.section_gap;
            day += Duration::days(1);
        }

        let hits = rows.hits;
        ctx.scene.pop_layer();
        if !drew_any {
            center_text(
                ctx.scene,
                ctx.text,
                "Nothing scheduled — press New to add an event",
                14.0,
                colors.text_dim,
                area,
                ctx.transform,
            );
        }
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

/// One agenda paint pass: the clip area, the row metrics and the hit rects
/// the rows push, shared by every heading and row it paints.
struct RowPainter<'a> {
    ctx: PaintCtx<'a>,
    area: Rect,
    x0: f64,
    show_location: bool,
    pointer: Point,
    m: AgendaMetrics,
    hits: Vec<(Rect, AgendaAction)>,
}

impl RowPainter<'_> {
    /// Draw one day's two-line heading (relative label + long date) at `y`,
    /// clipped to `area`.
    fn draw_day_heading(&mut self, y: f64, day: NaiveDate, today: NaiveDate) {
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = &mut self.ctx;
        let (transform, theme) = (*transform, *theme);
        let (area, x0, m) = (self.area, self.x0, &self.m);
        let colors = &theme.tokens.colors;
        if y + m.heading_h > area.y0 && y < area.y1 {
            let heading_color = if day == today {
                colors.accent[5]
            } else {
                colors.neutral[8]
            };
            let label = date::relative_label(day, today);
            text.draw_text(
                scene,
                &label,
                HEADING_TEXT,
                heading_color,
                transform * Affine::translate((x0, y)),
                None,
            );
            // The date follows the label on a shared baseline: its smaller line
            // box is bottom-aligned to the label's, and it starts at the authored
            // column or just after a long label, whichever is further right.
            let label_w = f64::from(text.measure(&label, HEADING_TEXT, None).0);
            let date_x = x0 + HEADING_DATE_X.max(label_w + m.check_gap);
            text.draw_text(
                scene,
                &date::fmt_day_long(day),
                HEADING_DATE_TEXT,
                colors.text_dim,
                transform * Affine::translate((date_x, y + m.heading_text_h - m.heading_date_h)),
                None,
            );
        }
    }

    /// Draw one event row at `y`, pushing its hit-rect into `hits` when the row
    /// is on screen.
    fn draw_event_row(&mut self, y: f64, event: &crate::model::Event) {
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = &mut self.ctx;
        let (transform, theme) = (*transform, *theme);
        let (area, x0, show_location, pointer, m) = (
            self.area,
            self.x0,
            self.show_location,
            self.pointer,
            &self.m,
        );
        let hits = &mut self.hits;
        let colors = &theme.tokens.colors;
        let row = Rect::new(x0, y, area.x1 - m.side_pad, y + m.row_h);
        if row.y1 > area.y0 && row.y0 < area.y1 {
            if row.contains(pointer) {
                fill_round(scene, transform, row, 6.0, colors.lift(0.35));
            }
            fill_round(
                scene,
                transform,
                Rect::new(
                    row.x0,
                    row.y0 + m.bar_inset,
                    row.x0 + m.bar_inset,
                    row.y1 - m.bar_inset,
                ),
                2.0,
                event.color.color(theme),
            );
            // Every text run is centred on the row from its own measured line
            // height, so time, title and location share one visual centre.
            let meta_y = y + (m.row_h - m.row_meta_h) / 2.0;
            let title_y = y + (m.row_h - m.row_text_h) / 2.0;
            let time = if event.all_day {
                ALL_DAY_LABEL.to_string()
            } else {
                date::fmt_time(&event.start)
            };
            text.draw_text(
                scene,
                &time,
                ROW_META_TEXT,
                colors.text_dim,
                transform * Affine::translate((row.x0 + m.time_x, meta_y)),
                None,
            );
            // The location is a single elided line in a trailing column that is
            // never wider than its share of the row — wrapping it used to spill a
            // second line into the next row at larger text scales.
            let title_x = row.x0 + m.title_x;
            let mut title_right = row.x1 - m.check_gap;
            if show_location && !event.location.is_empty() {
                let loc_max = (row.width() * LOCATION_MAX_SHARE).max(40.0);
                let location = text.elide_to_width(
                    &event.location,
                    ROW_META_TEXT,
                    ui::num::f32_of_f64(loc_max),
                );
                let loc_w = f64::from(text.measure(&location, ROW_META_TEXT, None).0);
                let loc_x = row.x1 - m.bar_inset - loc_w;
                text.draw_text(
                    scene,
                    &location,
                    ROW_META_TEXT,
                    colors.text_dim,
                    transform * Affine::translate((loc_x, meta_y)),
                    None,
                );
                title_right = loc_x - m.check_gap;
            }
            let title_max = (title_right - title_x).max(40.0);
            let title = text.elide_to_width(&event.title, ROW_TEXT, ui::num::f32_of_f64(title_max));
            text.draw_text(
                scene,
                &title,
                ROW_TEXT,
                colors.neutral[9],
                transform * Affine::translate((title_x, title_y)),
                None,
            );
            if let Some(hit) = clamp_hit(row, area) {
                hits.push((hit, AgendaAction::OpenEvent(event.id)));
            }
        }
    }

    /// Draw one task row at `y`, pushing its open- and toggle-hit rects into
    /// `hits` when the row is on screen.
    fn draw_task_row(&mut self, y: f64, task: &crate::model::Task) {
        let PaintCtx {
            scene,
            transform,
            text,
            theme,
        } = &mut self.ctx;
        let (transform, theme) = (*transform, *theme);
        let (area, x0, pointer, m) = (self.area, self.x0, self.pointer, &self.m);
        let hits = &mut self.hits;
        let colors = &theme.tokens.colors;
        let row = Rect::new(x0, y, area.x1 - m.side_pad, y + m.row_h);
        if row.y1 > area.y0 && row.y0 < area.y1 {
            if row.contains(pointer) {
                fill_round(scene, transform, row, 6.0, colors.lift(0.35));
            }
            if let Some(hit) = clamp_hit(row, area) {
                hits.push((hit, AgendaAction::OpenTask(task.id)));
            }
            // The checkbox is one text line tall and centred on the row, so it
            // stays proportionate to the label it toggles at every text scale.
            let half = m.check_size / 2.0;
            let checkbox = Rect::new(
                row.x0 + m.check_gap,
                row.center().y - half,
                row.x0 + m.check_gap + m.check_size,
                row.center().y + half,
            );
            draw_checkbox(scene, transform, theme, checkbox, task.done);
            let label_color = if task.done {
                colors.text_dim
            } else {
                colors.neutral[9]
            };
            let label_x0 = checkbox.x1 + m.check_gap;
            let title_y = y + (m.row_h - m.row_text_h) / 2.0;
            // Reserve the trailing flag column from the measured flag glyph so the
            // label can never run underneath it.
            let flag = (task.priority != Priority::None).then(|| task.priority.flag());
            let flag_w = flag.map_or(0.0, |f| f64::from(text.measure(f, ROW_FLAG_TEXT, None).0));
            let flag_x = row.x1 - m.check_gap - flag_w;
            let label_max = (flag_x - m.check_gap - label_x0).max(40.0);
            let label = text.elide_to_width(&task.title, ROW_TEXT, ui::num::f32_of_f64(label_max));
            text.draw_text(
                scene,
                &label,
                ROW_TEXT,
                label_color,
                transform * Affine::translate((label_x0, title_y)),
                None,
            );
            let label_width = f64::from(text.measure(&label, ROW_TEXT, None).0);
            if task.done {
                stroke_line(
                    scene,
                    transform,
                    colors.text_dim,
                    Point::new(label_x0, row.center().y),
                    Point::new(label_x0 + label_width, row.center().y),
                    1.0,
                );
            }
            // The toggle target covers the checkbox and the label, but stops short
            // of the trailing open-task region: a long title must still leave room
            // on the row that opens the task instead of completing it.
            let toggle_right = (label_x0 + label_width + m.check_gap)
                .min(flag_x - m.check_gap)
                .min(row.x1 - TASK_OPEN_MIN_W)
                .max(checkbox.x1 + m.check_gap);
            let complete = Rect::new(row.x0, row.y0, toggle_right, row.y1);
            if let Some(hit) = clamp_hit(complete, area) {
                hits.push((hit, AgendaAction::ToggleTask(task.id)));
            }
            if let Some(flag) = flag {
                text.draw_text(
                    scene,
                    flag,
                    ROW_FLAG_TEXT,
                    task.priority.color(theme),
                    transform * Affine::translate((flag_x, y + (m.row_h - m.row_flag_h) / 2.0)),
                    None,
                );
            }
        }
    }
}

/// What the agenda lists: the model's events and top-level tasks from
/// `anchor` onward, with hidden calendars and the calendar filter applied.
pub(crate) struct AgendaSource<'a> {
    pub(crate) model: &'a Model,
    pub(crate) anchor: NaiveDate,
    pub(crate) hidden_calendars: &'a HashSet<String>,
    pub(crate) calendar_filter: Option<&'a str>,
}

impl<'a> AgendaSource<'a> {
    fn events_on(&self, day: NaiveDate) -> Vec<&'a crate::model::Event> {
        visible_events_on(self.model, day, self.hidden_calendars, self.calendar_filter)
    }

    fn tasks_on(&self, day: NaiveDate) -> Vec<&'a crate::model::Task> {
        self.model
            .tasks
            .iter()
            .filter(|task| task.due == Some(day) && task.parent.is_none())
            .collect()
    }
}

fn visible_events_on<'a>(
    model: &'a Model,
    date: NaiveDate,
    hidden_calendars: &HashSet<String>,
    calendar_filter: Option<&str>,
) -> Vec<&'a crate::model::Event> {
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

fn content_height(source: &AgendaSource<'_>, m: &AgendaMetrics) -> f64 {
    let end = source.anchor + Duration::days(DAYS_SHOWN);
    let mut height = TOP_PAD;
    let mut day = source.anchor;
    while day <= end {
        let rows = source.events_on(day).len() + source.tasks_on(day).len();
        if rows > 0 {
            height += m.heading_h + m.row_pitch * rows as f64 + m.section_gap;
        }
        day += Duration::days(1);
    }
    height + m.bottom_pad
}

fn clamp_hit(row: Rect, viewport: Rect) -> Option<Rect> {
    let hit = row.intersect(viewport);
    (hit.area() > 0.0).then_some(hit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Event, Palette, Recurrence, Task};
    use uuid::Uuid;

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, day).expect("valid fixture date")
    }

    fn source<'a>(
        model: &'a Model,
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
            all_day: false,
            location: String::new(),
            color: Palette::Indigo,
            recurrence: Recurrence::None,
            recurrence_interval: 1,
            recurrence_until: None,
            recurrence_count: None,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: None,
            attendees: Vec::new(),
            reminder_min: None,
            project: None,
            calendar_id: calendar_id.map(str::to_owned),
        }
    }

    #[test]
    fn content_extent_and_visible_events_respect_hidden_and_selected_calendars() {
        // Case `content_extent_matches_headers_and_rows`.
        {
            let anchor = date(1);
            let model = Model {
                events: vec![event(1, None)],
                tasks: vec![Task {
                    id: Uuid::new_v4(),
                    title: "Task".into(),
                    done: false,
                    due: Some(anchor),
                    priority: Priority::None,
                    project: None,
                    parent: None,
                }],
                ..Model::default()
            };
            let m = AgendaMetrics::new(&Spacing::default(), 1.0);
            // 18 top pad + 26 heading + 2 × 32 rows + 12 section gap + 8 bottom.
            assert_eq!(
                content_height(&source(&model, anchor, &HashSet::new()), &m),
                128.0
            );
        }
        // Case `hidden_remote_calendars_do_not_expand_the_scroll_extent`.
        {
            let anchor = date(1);
            let model = Model {
                events: vec![event(1, Some("hidden"))],
                ..Model::default()
            };
            let hidden = HashSet::from(["hidden".to_string()]);
            let m = AgendaMetrics::new(&Spacing::default(), 1.0);
            assert_eq!(content_height(&source(&model, anchor, &hidden), &m), 26.0);
            assert_eq!(
                content_height(&source(&model, anchor, &HashSet::new()), &m),
                96.0
            );
        }
        // Case `selected_calendar_filters_events_without_overriding_visibility`.
        {
            let anchor = date(1);
            let model = Model {
                events: vec![
                    event(1, None),
                    event(1, Some("work")),
                    event(1, Some("home")),
                ],
                ..Model::default()
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
    fn metrics_reproduce_authored_rows_at_one_x_and_grow_on_whole_pixels() {
        // Case `one_x_metrics_reproduce_the_authored_rows`.
        {
            let m = AgendaMetrics::new(&Spacing::default(), 1.0);
            // The authored rows win at 1×: 18.75 + `s1` and 16.25 + `s3` both
            // fit inside 26 and 30, so nothing drifts off whole pixels.
            assert_eq!(m.heading_h, 26.0);
            assert_eq!(m.row_h, 30.0);
            assert_eq!(m.row_pitch, 32.0);
            assert_eq!(m.title_x, 90.0);
            assert_eq!(m.check_size, 16.0);
            let sp = Spacing::default();
            assert_eq!(m.side_pad, f64::from(sp.s6));
            assert_eq!(m.bottom_pad, f64::from(sp.s2));
        }
        // Case `scaled_rows_land_on_whole_pixels`:
        // Scaled rows are whole pixels too: a 2× row is the authored row plus
        // its line's growth, snapped up, never a fraction.
        {
            let m = AgendaMetrics::new(&Spacing::default(), 2.0);
            for value in [m.heading_h, m.row_h, m.row_pitch] {
                assert_eq!(value.fract(), 0.0, "{value} is not a whole pixel");
            }
            // Each row is its scaled line plus the same theme pad the painter
            // leaves, rounded up — never a fraction short of it.
            let sp = Spacing::default();
            assert_eq!(m.row_h, (m.row_text_h + f64::from(sp.s3)).ceil());
            assert_eq!(m.heading_h, (m.heading_text_h + f64::from(sp.s1)).ceil());
        }
        // Case `scaled_rows_hold_their_text_and_grow_the_scroll_extent`.
        {
            let m1 = AgendaMetrics::new(&Spacing::default(), 1.0);
            let m2 = AgendaMetrics::new(&Spacing::default(), 2.0);
            // A 2× row must hold a 2× line with the same padding a 1× row has.
            assert!(m2.row_h >= m2.row_text_h + 12.0);
            assert!(m2.heading_h >= m2.heading_text_h + 4.0);
            assert!(m2.row_h > m1.row_h && m2.heading_h > m1.heading_h);
            assert!(m2.check_size > m1.check_size);

            let anchor = date(1);
            let model = Model {
                events: vec![event(1, None)],
                ..Model::default()
            };
            let one = content_height(&source(&model, anchor, &HashSet::new()), &m1);
            let two = content_height(&source(&model, anchor, &HashSet::new()), &m2);
            assert_eq!(
                two - one,
                (m2.heading_h - m1.heading_h) + (m2.row_pitch - m1.row_pitch)
            );
        }
    }
}
