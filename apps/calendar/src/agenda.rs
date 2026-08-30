//! Agenda list component.
//!
//! This owns the agenda view's scroll state, geometry, input routing, drawing,
//! and semantic row actions. `CalendarApp` supplies the model and translates
//! those actions into its document mutations/navigation.

use std::collections::HashSet;

use automerge::{AutoCommit, ObjId};
use chrono::{Duration, NaiveDate};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;
use winit::event::WindowEvent;

use ui::Widget;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Scrollable;

use crate::app::{center_text, draw_checkbox, fill_rect, fill_round, stroke_line};
use crate::date;
use crate::model::{Id, Model, Priority};

const DAYS_SHOWN: i64 = 21;

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
        model: &Model,
        anchor: NaiveDate,
        hidden_calendars: &HashSet<String>,
    ) {
        self.scroll.set_viewport_rect(area);
        self.scroll.set_content_size(
            area.width() as f32,
            content_height(model, anchor, hidden_calendars) as f32,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut handled = false;
        let mut next_focus = *focus_target;
        self.scroll.handle_event(
            event,
            scale_factor,
            modifiers,
            state,
            path,
            &mut handled,
            &mut next_focus,
            node_id,
            taffy,
        );
        if handled {
            *focus_target = next_focus;
        }
        handled
    }

    pub(crate) fn wants_pointer_capture(&self) -> bool {
        self.scroll.wants_pointer_capture()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        model: &Model,
        anchor: NaiveDate,
        today: NaiveDate,
        hidden_calendars: &HashSet<String>,
        pointer: Point,
    ) -> Vec<(Rect, AgendaAction)> {
        let colors = &theme.tokens.colors;
        let mut hits = Vec::new();
        fill_rect(scene, transform, colors.surfaces.base, area);

        let end = anchor + Duration::days(DAYS_SHOWN);
        let x0 = area.x0 + 24.0;
        let show_location = area.width() >= 520.0;
        let mut y = area.y0 + 18.0 - f64::from(self.scroll.offset_y());
        let mut drew_any = false;

        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &area,
        );

        let mut day = anchor;
        while day <= end {
            let events = visible_events_on(model, day, hidden_calendars);
            let tasks: Vec<_> = model
                .tasks
                .iter()
                .filter(|task| task.due == Some(day) && task.parent.is_none())
                .collect();
            if events.is_empty() && tasks.is_empty() {
                day += Duration::days(1);
                continue;
            }
            drew_any = true;

            if y + 24.0 > area.y0 && y < area.y1 {
                let heading_color = if day == today {
                    colors.accent[5]
                } else {
                    colors.neutral[8]
                };
                text.draw_text(
                    scene,
                    &date::relative_label(day, today),
                    15.0,
                    heading_color,
                    transform * Affine::translate((x0, y)),
                    None,
                );
                text.draw_text(
                    scene,
                    &date::fmt_day_long(day),
                    12.0,
                    colors.text_dim,
                    transform * Affine::translate((x0 + 120.0, y + 2.0)),
                    None,
                );
            }
            y += 26.0;

            for event in events {
                let row = Rect::new(x0, y, area.x1 - 24.0, y + 30.0);
                if row.y1 > area.y0 && row.y0 < area.y1 {
                    if row.contains(pointer) {
                        fill_round(scene, transform, colors.surface_white_5, row, 6.0);
                    }
                    fill_round(
                        scene,
                        transform,
                        event.color.color(theme),
                        Rect::new(row.x0, row.y0 + 4.0, row.x0 + 4.0, row.y1 - 4.0),
                        2.0,
                    );
                    let time = if event.all_day {
                        "All day".to_string()
                    } else {
                        date::fmt_time(&event.start)
                    };
                    text.draw_text(
                        scene,
                        &time,
                        12.0,
                        colors.text_dim,
                        transform * Affine::translate((row.x0 + 16.0, y + 7.0)),
                        None,
                    );
                    let title_max = if show_location && !event.location.is_empty() {
                        row.width() - 220.0
                    } else {
                        row.width() - 104.0
                    };
                    text.draw_text(
                        scene,
                        &event.title,
                        13.0,
                        colors.neutral[9],
                        transform * Affine::translate((row.x0 + 90.0, y + 7.0)),
                        Some(title_max.max(40.0) as f32),
                    );
                    if show_location && !event.location.is_empty() {
                        text.draw_text(
                            scene,
                            &event.location,
                            12.0,
                            colors.text_dim,
                            transform * Affine::translate((row.x1 - 130.0, y + 7.0)),
                            Some(126.0),
                        );
                    }
                    if let Some(hit) = clamp_hit(row, area) {
                        hits.push((hit, AgendaAction::OpenEvent(event.id)));
                    }
                }
                y += 32.0;
            }

            for task in tasks {
                let row = Rect::new(x0, y, area.x1 - 24.0, y + 30.0);
                if row.y1 > area.y0 && row.y0 < area.y1 {
                    if row.contains(pointer) {
                        fill_round(scene, transform, colors.surface_white_5, row, 6.0);
                    }
                    if let Some(hit) = clamp_hit(row, area) {
                        hits.push((hit, AgendaAction::OpenTask(task.id)));
                    }
                    let checkbox = Rect::new(
                        row.x0 + 8.0,
                        row.center().y - 8.0,
                        row.x0 + 24.0,
                        row.center().y + 8.0,
                    );
                    draw_checkbox(scene, transform, theme, checkbox, task.done);
                    let label_color = if task.done {
                        colors.text_dim
                    } else {
                        colors.neutral[9]
                    };
                    let label_x0 = row.x0 + 34.0;
                    text.draw_text(
                        scene,
                        &task.title,
                        13.0,
                        label_color,
                        transform * Affine::translate((label_x0, y + 7.0)),
                        Some((row.width() - 160.0).max(40.0) as f32),
                    );
                    let label_width = f64::from(text.measure(&task.title, 13.0, None).0);
                    if task.done {
                        stroke_line(
                            scene,
                            transform,
                            colors.text_dim,
                            Point::new(label_x0, row.center().y),
                            Point::new(
                                (label_x0 + label_width).min(row.x1 - 120.0),
                                row.center().y,
                            ),
                            1.0,
                        );
                    }
                    let complete = Rect::new(
                        row.x0,
                        row.y0,
                        (label_x0 + label_width.min(row.width() - 160.0) + 8.0).min(row.x1 - 40.0),
                        row.y1,
                    );
                    if let Some(hit) = clamp_hit(complete, area) {
                        hits.push((hit, AgendaAction::ToggleTask(task.id)));
                    }
                    if task.priority != Priority::None {
                        text.draw_text(
                            scene,
                            task.priority.flag(),
                            11.0,
                            task.priority.color(theme),
                            transform * Affine::translate((row.x1 - 30.0, y + 7.0)),
                            None,
                        );
                    }
                }
                y += 32.0;
            }
            y += 12.0;
            day += Duration::days(1);
        }

        scene.pop_layer();
        if !drew_any {
            center_text(
                scene,
                text,
                "Nothing scheduled — press New to add an event",
                14.0,
                colors.text_dim,
                area,
                transform,
            );
        }
        self.scroll.draw_bar_overlay(scene, transform, theme);
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

fn visible_events_on<'a>(
    model: &'a Model,
    date: NaiveDate,
    hidden_calendars: &HashSet<String>,
) -> Vec<&'a crate::model::Event> {
    model
        .events_on(date)
        .into_iter()
        .filter(|event| event.visible_under(hidden_calendars))
        .collect()
}

fn content_height(model: &Model, anchor: NaiveDate, hidden_calendars: &HashSet<String>) -> f64 {
    let end = anchor + Duration::days(DAYS_SHOWN);
    let mut height = 18.0;
    let mut day = anchor;
    while day <= end {
        let events = visible_events_on(model, day, hidden_calendars).len();
        let tasks = model
            .tasks
            .iter()
            .filter(|task| task.due == Some(day) && task.parent.is_none())
            .count();
        if events + tasks > 0 {
            height += 26.0 + 32.0 * (events + tasks) as f64 + 12.0;
        }
        day += Duration::days(1);
    }
    height + 8.0
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
            reminder_min: None,
            project: None,
            calendar_id: calendar_id.map(str::to_owned),
        }
    }

    #[test]
    fn content_extent_matches_headers_and_rows() {
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
        assert_eq!(content_height(&model, anchor, &HashSet::new()), 128.0);
    }

    #[test]
    fn hidden_remote_calendars_do_not_expand_the_scroll_extent() {
        let anchor = date(1);
        let model = Model {
            events: vec![event(1, Some("hidden"))],
            ..Model::default()
        };
        let hidden = HashSet::from(["hidden".to_string()]);
        assert_eq!(content_height(&model, anchor, &hidden), 26.0);
        assert_eq!(content_height(&model, anchor, &HashSet::new()), 96.0);
    }
}
