//! Tasks list component.
//!
//! The Tasks screen lists the sidebar's selected list (a smart list or a
//! project) in full: a heading with the list's name, the field that adds a
//! task to it, and one row per task with its subtasks indented beneath it.
//! The component owns the scroll state, the entry field, geometry, drawing
//! and the row actions; `CalendarApp` supplies the model and turns those
//! actions into the calendar's edits and navigation, through the same paths
//! the agenda and the sidebar rail use.

use chrono::{Duration, NaiveDate};
use ui::Widget;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::widget::{EmptyState, EmptyStateContent, Scrollable, TextInput};
use vello::kurbo::{Point, Rect};
use vello::peniko::Fill;

use crate::date;
use crate::input_context::InputContext;
use crate::model::{Calendar, Id, ListSel, Priority, Task};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{ControlSize, Interaction, ListRowFace, RowLeading, Tone};

/// Nesting deeper than this is drawn at this depth, so a malformed parent
/// chain can neither recurse forever nor push a row off the screen.
const MAX_DEPTH: usize = 4;

/// What a press on the Tasks screen asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TasksAction {
    OpenTask(Id),
    ToggleTask(Id),
    FocusEntry,
}

/// The list the Tasks screen shows.
pub(crate) struct TasksSource<'a> {
    pub(crate) model: &'a Calendar,
    pub(crate) list: &'a ListSel,
    pub(crate) today: NaiveDate,
}

/// One painted row: a task and how deep it sits under its parents.
pub(crate) struct TaskRow<'a> {
    pub(crate) task: &'a Task,
    pub(crate) depth: usize,
}

impl<'a> TasksSource<'a> {
    /// The list's tasks in display order, each followed by its subtasks.
    pub(crate) fn rows(&self) -> Vec<TaskRow<'a>> {
        let mut rows = Vec::new();
        for task in self.model.tasks_for(self.list, self.today) {
            self.push_with_subtasks(&mut rows, task, 0);
        }
        rows
    }

    fn push_with_subtasks(&self, rows: &mut Vec<TaskRow<'a>>, task: &'a Task, depth: usize) {
        rows.push(TaskRow { task, depth });
        if depth >= MAX_DEPTH {
            return;
        }
        for sub in self.model.subtasks_of(task.id) {
            self.push_with_subtasks(rows, sub, depth + 1);
        }
    }

    pub(crate) fn title(&self) -> String {
        let key = match self.list {
            ListSel::Inbox => "calendar-tasks-inbox",
            ListSel::Today => "calendar-tasks-today",
            ListSel::Upcoming => "calendar-tasks-upcoming",
            ListSel::Completed => "calendar-tasks-completed",
            ListSel::Project(_) => return self.model.list_title(self.list),
        };
        crate::resource::t(key)
    }

    /// Whether a task can be added to this list: a new task is open, so it
    /// never belongs to Completed.
    pub(crate) fn accepts_new(&self) -> bool {
        !matches!(self.list, ListSel::Completed)
    }

    /// Where a task added to this list is filed so it shows up in it: under
    /// the project, due today for Today, due tomorrow for Upcoming.
    pub(crate) fn placement(&self) -> (Option<Id>, Option<NaiveDate>) {
        match self.list {
            ListSel::Project(id) => (Some(*id), None),
            ListSel::Today => (None, Some(self.today)),
            ListSel::Upcoming => (None, Some(self.today + Duration::days(1))),
            ListSel::Inbox | ListSel::Completed => (None, None),
        }
    }

    /// The empty state's title and explanation.
    pub(crate) fn empty_copy(&self) -> (String, String) {
        let key = match self.list {
            ListSel::Inbox => "calendar-tasks-empty-inbox",
            ListSel::Today => "calendar-tasks-empty-today",
            ListSel::Upcoming => "calendar-tasks-empty-upcoming",
            ListSel::Completed => "calendar-tasks-empty-completed",
            ListSel::Project(_) => "calendar-tasks-empty-project",
        };
        let title =
            crate::resource::t_args(key, &[("list", ui::i18n::ArgValue::from(self.title()))]);
        let message = crate::resource::t(if matches!(self.list, ListSel::Completed) {
            "calendar-tasks-empty-completed-hint"
        } else {
            "calendar-tasks-empty-hint"
        });
        (title, message)
    }
}

/// The screen's regions, solved once per layout from the same metrics the
/// painter and the hit rects read.
#[derive(Clone, Copy, Debug, Default)]
struct Regions {
    heading: Rect,
    entry: Option<Rect>,
    list: Rect,
}

fn regions(area: Rect, theme: &Theme, accepts_new: bool) -> Regions {
    let gutter = f64::from(theme.tokens.spacing.s6);
    let gap = f64::from(theme.tokens.spacing.rhythm.related);
    let heading_h = ui::text_render::with_layout_renderer(theme.text_scale, |text| {
        f64::from(
            text.measure_style("Ag", None, &theme.text(TextRole::Subheading))
                .1,
        )
    });
    let x0 = area.x0 + gutter;
    let x1 = (area.x1 - gutter).max(x0);
    let heading = Rect::new(x0, area.y0 + gutter, x1, area.y0 + gutter + heading_h);
    let mut y = heading.y1 + gap;
    let entry = accepts_new.then(|| {
        let rect = Rect::new(x0, y, x1, y + ControlSize::Regular.height(theme));
        y = rect.y1 + f64::from(theme.tokens.spacing.rhythm.group);
        rect
    });
    Regions {
        heading,
        entry,
        list: Rect::new(area.x0, y.min(area.y1), area.x1, area.y1),
    }
}

fn row_face<'a>(task: &'a Task, due: &'a str, spans: &'a [(&'a str, Tone)]) -> ListRowFace<'a> {
    let face = ListRowFace::new(&task.title)
        .leading(RowLeading::Checkbox { checked: task.done })
        .meta_spans(spans);
    let _ = due;
    face
}

fn content_height(source: &TasksSource<'_>, theme: &Theme) -> f64 {
    ui::text_render::with_layout_renderer(theme.text_scale, |text| {
        source
            .rows()
            .iter()
            .map(|row| {
                let due = row
                    .task
                    .due
                    .map(|d| date::relative_label(d, source.today))
                    .unwrap_or_default();
                row_face(row.task, &due, &[]).height(text, theme)
            })
            .sum::<f64>()
            + f64::from(theme.tokens.spacing.s6)
    })
}

pub(crate) struct TasksView {
    scroll: Scrollable,
    entry: TextInput,
    regions: Regions,
}

impl TasksView {
    pub(crate) fn new() -> Self {
        Self {
            scroll: Scrollable::new_vertical().always_visible(),
            entry: TextInput::local()
                .with_placeholder(crate::resource::t("calendar-tasks-entry"))
                .with_text_role(TextRole::Body)
                .with_accessible_name(crate::resource::t("calendar-tasks-entry-name"))
                .with_enter_submits(true)
                .with_id("task_entry"),
            regions: Regions::default(),
        }
    }

    pub(crate) fn layout(&mut self, area: Rect, source: &TasksSource<'_>, theme: &Theme) {
        self.regions = regions(area, theme, source.accepts_new());
        self.scroll.set_viewport_rect(self.regions.list);
        self.scroll.set_content_size(
            self.regions.list.width() as f32,
            content_height(source, theme) as f32,
        );
        match self.regions.entry {
            Some(rect) => self.entry.apply_layout_external(rect),
            None => self.park_entry(),
        }
    }

    /// Move the entry field off-screen and release it, while another screen
    /// or a list that takes no new task is shown.
    pub(crate) fn park_entry(&mut self) {
        self.regions.entry = None;
        self.entry.set_focus(false);
        self.entry.apply_layout_external(Rect::ZERO);
    }

    /// The scrolling rows' viewport.
    pub(crate) fn list_rect(&self) -> Rect {
        self.regions.list
    }

    pub(crate) fn entry_rect(&self) -> Option<Rect> {
        self.regions.entry
    }

    pub(crate) fn entry_focused(&self) -> bool {
        self.entry.is_focused()
    }

    /// Focus or release the entry field. A list that takes no new task
    /// releases it again at its next layout.
    pub(crate) fn set_entry_focus(&mut self, focused: bool) {
        self.entry.set_focus(focused);
    }

    /// Route one event to the focused entry field; answers the title typed
    /// when the event submitted it, and clears the field for the next one.
    pub(crate) fn handle_entry_event(&mut self, input: &mut InputContext<'_>) -> Option<String> {
        self.entry.handle_event(
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
        if !self.entry.take_submit() {
            return None;
        }
        let title = self.entry.value().trim().to_owned();
        self.entry.set_text("");
        (!title.is_empty()).then_some(title)
    }

    /// Route wheel and scrollbar input to the rows' scroller.
    pub(crate) fn handle_scroll_event(&mut self, input: &mut InputContext<'_>) -> bool {
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
        &mut self,
        ctx: &mut PaintCtx<'_>,
        source: &TasksSource<'_>,
        pointer: Point,
        presence: &PresenceCtx,
    ) -> Vec<(Rect, TasksAction)> {
        let theme = ctx.theme;
        let rows = source.rows();
        let mut hits = Vec::new();

        self.draw_heading(ctx, source);
        if let Some(entry) = self.regions.entry {
            self.entry
                .set_placeholder(&crate::resource::t("calendar-tasks-entry"));
            self.entry
                .draw(ctx.scene, ctx.transform, ctx.text, theme, presence);
            hits.push((entry, TasksAction::FocusEntry));
        }

        let list = self.regions.list;
        if rows.is_empty() {
            let (title, message) = source.empty_copy();
            EmptyState::paint(
                ctx,
                list,
                &EmptyStateContent {
                    message: Some(&message),
                    ..EmptyStateContent::new(&title)
                },
                pointer,
            );
            return hits;
        }

        ctx.scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            ctx.transform,
            &list,
        );
        let gutter = f64::from(theme.tokens.spacing.s6);
        let indent_step = ControlSize::Regular.height(theme);
        let mut y = list.y0 - f64::from(self.scroll.offset_y());
        for row in &rows {
            let task = row.task;
            let due = task
                .due
                .map(|d| date::relative_label(d, source.today))
                .unwrap_or_default();
            let priority = crate::resource::t(match task.priority {
                Priority::None => "calendar-tasks-priority-none",
                Priority::Low => "calendar-tasks-priority-low",
                Priority::Medium => "calendar-tasks-priority-medium",
                Priority::High => "calendar-tasks-priority-high",
            });
            let tone = match task.priority {
                Priority::High => Tone::Danger,
                Priority::Medium => Tone::Warning,
                Priority::Low | Priority::None => Tone::Neutral,
            };
            let due_tone = if task.due.is_some_and(|d| d < source.today) && !task.done {
                Tone::Danger
            } else {
                Tone::Neutral
            };
            let mut spans = Vec::new();
            if !due.is_empty() {
                spans.push((due.as_str(), due_tone));
            }
            if task.priority != Priority::None {
                spans.push((priority.as_str(), tone));
            }
            let meta = spans.as_slice();
            let face = row_face(task, &due, meta);
            let h = face.height(ctx.text, theme);
            let x0 = list.x0 + gutter + indent_step * row.depth as f64;
            let rect = Rect::new(x0, y, (list.x1 - gutter).max(x0), y + h);
            if rect.y1 > list.y0 && rect.y0 < list.y1 {
                let face = face.interaction(Interaction {
                    hovered: rect.contains(pointer),
                    ..Interaction::default()
                });
                face.paint(ctx, rect);
                let hit = rect.intersect(list);
                if hit.area() > 0.0 {
                    hits.push((hit, TasksAction::OpenTask(task.id)));
                }
                let hit = face.leading_rect(rect, ctx.text, theme).intersect(list);
                if hit.area() > 0.0 {
                    hits.push((hit, TasksAction::ToggleTask(task.id)));
                }
            }
            y = rect.y1;
        }
        ctx.scene.pop_layer();
        self.scroll
            .draw_bar_overlay(ctx.scene, ctx.transform, theme);
        hits
    }

    /// Paint the selected list's heading using the shared text role.
    fn draw_heading(&self, ctx: &mut PaintCtx<'_>, source: &TasksSource<'_>) {
        let style = ctx.theme.text(TextRole::Subheading);
        let band = self.regions.heading;
        crate::draw_util::text_in_band(
            ctx,
            &source.title(),
            &style,
            ctx.theme.text_primary,
            band.x0,
            band,
            band.width(),
        );
    }

    pub(crate) fn needs_redraw(&self) -> bool {
        self.scroll.needs_redraw() || self.entry.needs_redraw()
    }

    pub(crate) fn mark_drawn(&mut self) {
        self.scroll.mark_drawn();
        self.entry.mark_drawn();
    }

    pub(crate) fn needs_animation_frame(&self) -> bool {
        self.scroll.needs_animation_frame()
    }

    pub(crate) fn is_layout_dirty(&self) -> bool {
        self.scroll.is_layout_dirty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, d).expect("valid fixture date")
    }

    fn task(title: &str, parent: Option<Id>, project: Option<Id>) -> Task {
        Task {
            id: Uuid::new_v4(),
            title: title.into(),
            done: false,
            due: None,
            priority: Priority::None,
            project,
            parent,
        }
    }

    #[test]
    fn rows_nest_subtasks_under_their_parent_and_place_new_tasks_in_the_list() {
        let project = Uuid::new_v4();
        let parent = task("Plan trip", None, Some(project));
        let child = task("Book flights", Some(parent.id), Some(project));
        let grandchild = task("Compare prices", Some(child.id), Some(project));
        let other = task("Groceries", None, None);
        let model = Calendar {
            tasks: [other, grandchild, child, parent].into_iter().collect(),
            ..Calendar::default()
        };
        let list = ListSel::Project(project);
        let source = TasksSource {
            model: &model,
            list: &list,
            today: day(1),
        };
        let rows: Vec<_> = source
            .rows()
            .iter()
            .map(|row| (row.task.title.as_str(), row.depth))
            .collect();
        assert_eq!(
            rows,
            vec![("Plan trip", 0), ("Book flights", 1), ("Compare prices", 2)]
        );
        assert_eq!(source.placement(), (Some(project), None));

        for (list, placement, accepts) in [
            (ListSel::Inbox, (None, None), true),
            (ListSel::Today, (None, Some(day(1))), true),
            (ListSel::Upcoming, (None, Some(day(2))), true),
            (ListSel::Completed, (None, None), false),
        ] {
            let source = TasksSource {
                model: &model,
                list: &list,
                today: day(1),
            };
            assert_eq!(source.placement(), placement, "{list:?}");
            assert_eq!(source.accepts_new(), accepts, "{list:?}");
        }
    }

    #[test]
    fn regions_stack_heading_entry_and_rows_and_drop_the_entry_for_completed() {
        let theme = Theme::default();
        let area = Rect::new(0.0, 60.0, 800.0, 700.0);
        let model = Calendar::default();
        let mut view = TasksView::new();
        view.layout(
            area,
            &TasksSource {
                model: &model,
                list: &ListSel::Inbox,
                today: day(1),
            },
            &theme,
        );
        let with_entry = view.regions;
        let entry = with_entry.entry.expect("an entry field");
        assert!(with_entry.heading.y1 < entry.y0);
        assert!(entry.y1 < with_entry.list.y0);
        assert_eq!(entry.height(), ControlSize::Regular.height(&theme));
        assert_eq!(with_entry.list.y1, area.y1);
        view.set_entry_focus(true);
        view.layout(
            area,
            &TasksSource {
                model: &model,
                list: &ListSel::Completed,
                today: day(1),
            },
            &theme,
        );
        assert!(view.entry_rect().is_none());
        assert!(!view.entry_focused());
        assert!(view.list_rect().y0 < with_entry.list.y0);
    }
}
