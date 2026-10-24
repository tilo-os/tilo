//! The event and task editor: a modal sheet presented in the window's top
//! layer through the shared modal chrome (`ui::widget::modal_dialog`). The
//! sheet's height follows its content; a form taller than the window scrolls
//! inside the sheet's body. Every field is a toolkit control: text inputs,
//! buttons that open the date and time pickers, a switch, dropdowns and
//! checkboxes, laid out as label-leading form rows.

use std::fmt::Write as _;

use chrono::NaiveDate;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Fill;

use ui::LocalState;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::modal_dialog::{self, ModalAction, ModalLayout, ModalSpec};
use ui::widget::{
    AvatarFace, Button, ButtonFace, ButtonRole, CheckGlyph, Checkbox, CheckboxFace,
    ColorSwatchFace, ControlSize, Dropdown, ListRowFace, RowLeading, Toggle, ToggleFace, Widget,
};

use super::{Action, CalTarget, CalendarApp, FormField, Sel, TaskPopover};
use crate::date;
use crate::draw_util::text_in_band;
use crate::event_popovers::{PopoverAnchors, PopoverKind as FormPopover};
use crate::model::{Event, Palette, Priority, Recurrence, Task};

const DONE: &str = "Done";

/// One dropdown of the editor with the value it last published, so a pick
/// is applied to the model exactly once.
pub(crate) struct Choice {
    pub(super) state: LocalState<String>,
    pub(super) dropdown: Dropdown,
    last: String,
    pub(super) rect: Rect,
}

impl Choice {
    /// A dropdown named `name` whose `icon` says what it chooses.
    pub(super) fn new(name: &str, icon: Icon) -> Self {
        let state = LocalState::new(String::new());
        let dropdown = Dropdown::from_state(Vec::new(), &state)
            .with_accessible_name(name)
            .leading_icon(icon);
        Self {
            state,
            dropdown,
            last: String::new(),
            rect: Rect::ZERO,
        }
    }

    /// Offer `options` with `value` chosen, unless the list is open.
    pub(super) fn seed(&mut self, options: Vec<(String, String)>, value: &str) {
        if self.dropdown.is_open() {
            return;
        }
        self.dropdown.set_value_options(options);
        if value != self.last || self.dropdown.selected() != value {
            self.state.set(value.to_string());
            self.dropdown.set_selected(value.to_string());
            self.last = value.to_string();
        }
    }

    /// The value the person just picked, once.
    pub(super) fn take_pick(
        &mut self,
        doc: &ui::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) -> Option<String> {
        self.dropdown.sync(doc.doc(), path);
        let value = self.state.get();
        (value != self.last).then(|| {
            self.last.clone_from(&value);
            value
        })
    }

    pub(super) fn place(&mut self, rect: Rect, window: Rect) {
        self.rect = rect;
        self.dropdown.apply_layout_external(rect);
        self.dropdown.set_viewport(window);
    }
}

/// The editor's dropdowns.
pub(crate) struct Choices {
    pub(super) calendar: Choice,
    pub(super) repeat: Choice,
    pub(super) alert: Choice,
    pub(super) timezone: Choice,
    pub(super) priority: Choice,
    pub(super) project: Choice,
}

impl Choices {
    pub(super) fn new() -> Self {
        Self {
            calendar: Choice::new("Calendar", Icon::Calendar),
            repeat: Choice::new("Repeat", Icon::Repeat),
            alert: Choice::new("Alert", Icon::Bell),
            timezone: Choice::new("Time zone", Icon::Globe),
            priority: Choice::new("Priority", Icon::Flag),
            project: Choice::new("Project", Icon::Folder),
        }
    }

    /// The dropdowns the current sheet shows, in form order.
    pub(super) fn shown_mut(&mut self, task: bool) -> Vec<&mut Choice> {
        if task {
            vec![&mut self.priority, &mut self.project]
        } else {
            vec![
                &mut self.calendar,
                &mut self.repeat,
                &mut self.alert,
                &mut self.timezone,
            ]
        }
    }

    pub(super) fn close_all(&mut self) {
        for choice in [
            &mut self.calendar,
            &mut self.repeat,
            &mut self.alert,
            &mut self.timezone,
            &mut self.priority,
            &mut self.project,
        ] {
            choice.dropdown.close();
        }
    }
}

/// Where every part of the sheet's body landed, already offset by the body's
/// scroll. Recomputed every layout; the paint pass, the hit map and the
/// embedded widgets all read it.
#[derive(Clone, Debug, Default)]
pub(super) struct FormRects {
    /// Leading labels of the form rows, by the row they label.
    pub(super) labels: Vec<(&'static str, Rect)>,
    pub(super) title: Rect,
    pub(super) start_date: Rect,
    pub(super) start_time: Rect,
    pub(super) end_date: Rect,
    pub(super) end_time: Rect,
    pub(super) all_day: Rect,
    /// Read-only scheduling feedback, present only when this event overlaps
    /// another saved one.
    pub(super) conflict: Rect,
    pub(super) location: Rect,
    pub(super) calendar: Rect,
    pub(super) repeat: Rect,
    pub(super) alert: Rect,
    pub(super) timezone: Rect,
    /// The Colour row, and one swatch per calendar colour along it.
    pub(super) colour: Rect,
    pub(super) swatches: Vec<(Palette, Rect)>,
    pub(super) invite: Rect,
    pub(super) guests: Vec<Rect>,
    pub(super) task_check: Rect,
    pub(super) task_title: Rect,
    pub(super) due: Rect,
    pub(super) priority: Rect,
    pub(super) project: Rect,
    pub(super) subtasks: Vec<Rect>,
    /// The "Notes" heading and where the notes editor starts.
    pub(super) notes_label: Rect,
    pub(super) notes_top: f64,
}

impl FormRects {
    /// The bottom edge of every row of the form, top to bottom: where a
    /// body that cannot show the whole form may end without cutting a row.
    fn row_bottoms(&self) -> Vec<f64> {
        let mut rows: Vec<f64> = [
            self.title,
            self.start_date,
            self.end_date,
            self.all_day,
            self.conflict,
            self.location,
            self.calendar,
            self.repeat,
            self.alert,
            self.timezone,
            self.colour,
            self.invite,
            self.task_title,
            self.due,
            self.priority,
            self.project,
        ]
        .into_iter()
        .chain(self.guests.iter().copied())
        .chain(self.subtasks.iter().copied())
        .filter(|rect| rect.area() > 0.0)
        .map(|rect| rect.y1)
        .collect();
        rows.sort_by(f64::total_cmp);
        rows
    }
}

/// The repeat rule as one choice: a standard rule's value, or `custom` for
/// one the form cannot otherwise express (an interval of four, an end).
fn repeat_value(event: &Event) -> &'static str {
    let ends = event.recurrence_count.is_some() || event.recurrence_until.is_some();
    match (event.recurrence, event.recurrence_interval.max(1), ends) {
        (Recurrence::None, _, _) => "none",
        (Recurrence::Daily, 1, false) => "daily",
        (Recurrence::Weekdays, 1, false) => "weekdays",
        (Recurrence::Weekly, 1, false) => "weekly",
        (Recurrence::Weekly, 2, false) => "fortnightly",
        (Recurrence::Monthly, 1, false) => "monthly",
        (Recurrence::Yearly, 1, false) => "yearly",
        _ => "custom",
    }
}

/// How an event's repeat rule reads in the form.
fn repeat_label(event: &Event) -> String {
    let interval = event.recurrence_interval.max(1);
    let unit = match event.recurrence {
        Recurrence::None => return Recurrence::None.label().into(),
        Recurrence::Weekdays => return Recurrence::Weekdays.label().into(),
        Recurrence::Daily => "day",
        Recurrence::Weekly => "week",
        Recurrence::Monthly => "month",
        Recurrence::Yearly => "year",
    };
    let mut label = if interval == 1 {
        format!("Every {unit}")
    } else {
        format!("Every {interval} {unit}s")
    };
    if let Some(count) = event.recurrence_count {
        let _ = write!(label, ", {count} times");
    } else if let Some(until) = event.recurrence_until {
        let _ = write!(label, " until {}", date::fmt_day_short(until));
    }
    label
}

fn repeat_options(event: &Event) -> Vec<(String, String)> {
    let mut options: Vec<(String, String)> = [
        ("none", "Does not repeat"),
        ("daily", "Every day"),
        ("weekdays", "Every weekday"),
        ("weekly", "Every week"),
        ("fortnightly", "Every 2 weeks"),
        ("monthly", "Every month"),
        ("yearly", "Every year"),
    ]
    .into_iter()
    .map(|(value, label)| (value.into(), label.into()))
    .collect();
    if repeat_value(event) == "custom" {
        options.push(("custom".into(), repeat_label(event)));
    }
    options
}

fn apply_repeat(event: &mut Event, value: &str) {
    let (recurrence, interval) = match value {
        "daily" => (Recurrence::Daily, 1),
        "weekdays" => (Recurrence::Weekdays, 1),
        "weekly" => (Recurrence::Weekly, 1),
        "fortnightly" => (Recurrence::Weekly, 2),
        "monthly" => (Recurrence::Monthly, 1),
        "yearly" => (Recurrence::Yearly, 1),
        "none" => (Recurrence::None, 1),
        _ => return,
    };
    event.recurrence = recurrence;
    event.recurrence_interval = interval;
}

pub(super) const ALERTS: [(i64, &str); 7] = [
    (0, "At time of event"),
    (5, "5 minutes before"),
    (10, "10 minutes before"),
    (15, "15 minutes before"),
    (30, "30 minutes before"),
    (60, "1 hour before"),
    (1440, "1 day before"),
];

fn alert_value(event: &Event) -> String {
    event
        .reminder_min
        .map_or_else(|| "none".into(), |minutes| minutes.to_string())
}

fn alert_options(event: &Event) -> Vec<(String, String)> {
    let mut options = vec![("none".to_string(), "None".to_string())];
    options.extend(
        ALERTS
            .iter()
            .map(|(minutes, label)| (minutes.to_string(), (*label).to_string())),
    );
    if let Some(minutes) = event.reminder_min
        && !ALERTS.iter().any(|(m, _)| *m == minutes)
    {
        options.push((minutes.to_string(), format!("{minutes} minutes before")));
    }
    options
}

const TIME_ZONES: [&str; 5] = [
    "floating",
    "UTC",
    "Europe/Berlin",
    "America/New_York",
    "Asia/Tokyo",
];

/// A time zone as a person reads it: the city of an IANA name, or "No time
/// zone" for a floating event (one that keeps its wall time everywhere).
fn timezone_label(zone: &str) -> String {
    match zone {
        "floating" | "" => "No time zone".into(),
        "UTC" => "UTC".into(),
        other => other.rsplit('/').next().unwrap_or(other).replace('_', " "),
    }
}

fn timezone_options(event: &Event) -> Vec<(String, String)> {
    let mut options: Vec<(String, String)> = TIME_ZONES
        .iter()
        .map(|zone| ((*zone).to_string(), timezone_label(zone)))
        .collect();
    if !TIME_ZONES.contains(&event.timezone.as_str()) {
        options.push((event.timezone.clone(), timezone_label(&event.timezone)));
    }
    options
}

fn priority_value(priority: Priority) -> &'static str {
    match priority {
        Priority::High => "p1",
        Priority::Medium => "p2",
        Priority::Low => "p3",
        Priority::None => "none",
    }
}

fn priority_options() -> Vec<(String, String)> {
    [
        Priority::High,
        Priority::Medium,
        Priority::Low,
        Priority::None,
    ]
    .into_iter()
    .map(|priority| (priority_value(priority).into(), priority.label().into()))
    .collect()
}

/// The labels of the event form's rows, measured once for the label column.
const EVENT_LABELS: [&str; 10] = [
    "Starts",
    "Ends",
    "All day",
    "Location",
    "Calendar",
    "Repeat",
    "Alert",
    "Time zone",
    "Colour",
    "Guests",
];
const TASK_LABELS: [&str; 3] = ["Due", "Priority", "Project"];

/// Running geometry of one form layout pass.
struct Flow<'a> {
    theme: &'a Theme,
    body: Rect,
    field_x: f64,
    y: f64,
    row_h: f64,
    /// Labels sit above their fields (a narrow sheet) instead of beside them;
    /// `label_h` is the line a label takes then.
    stacked: bool,
    label_h: f64,
    labels: Vec<(&'static str, Rect)>,
}

impl<'a> Flow<'a> {
    /// A flow down `body` whose labels take `label_column` beside their
    /// fields, or stack above them when the fields would then be narrower
    /// than `widest_field`.
    fn new(
        theme: &'a Theme,
        body: Rect,
        label_column: f64,
        widest_field: f64,
        text: &mut TextRenderer,
    ) -> Self {
        let stacked = body.width() - label_column < widest_field;
        let label_h = f64::from(
            text.measure_style("Ag", None, &theme.text(TextRole::Body))
                .1,
        );
        Self {
            theme,
            body,
            field_x: if stacked {
                body.x0
            } else {
                body.x0 + label_column
            },
            y: body.y0,
            row_h: ControlSize::Regular.height(theme),
            stacked,
            label_h,
            labels: Vec::new(),
        }
    }

    /// A labelled row of `height`: the label's band and the field's span.
    fn row(&mut self, label: &'static str, height: f64) -> Rect {
        if self.stacked {
            let band = Rect::new(self.body.x0, self.y, self.body.x1, self.y + self.label_h);
            self.labels.push((label, band));
            self.y = band.y1 + f64::from(self.theme.tokens.spacing.rhythm.tight);
        }
        let band = Rect::new(self.body.x0, self.y, self.body.x1, self.y + height);
        if !self.stacked {
            self.labels.push((
                label,
                Rect::new(band.x0, band.y0, self.field_x, band.y0 + self.row_h),
            ));
        }
        self.y = band.y1 + f64::from(self.theme.tokens.spacing.rhythm.related);
        Rect::new(self.field_x, band.y0, band.x1, band.y1)
    }

    fn gap(&mut self) {
        self.y += f64::from(self.theme.tokens.spacing.rhythm.section)
            - f64::from(self.theme.tokens.spacing.rhythm.related);
    }

    fn control(&self, x0: f64, y0: f64, width: f64) -> Rect {
        Rect::new(x0, y0, x0 + width, y0 + self.row_h)
    }
}

impl CalendarApp {
    /// The sheet's title: what is being edited.
    pub(super) fn sheet_title(&self) -> &'static str {
        match self.selection {
            Some(Sel::Task(_)) => "Task",
            Some(Sel::Event(_)) if self.fresh_draft.is_some() => "New event",
            _ => "Event",
        }
    }

    /// The modal sheet's spec: its title and its one action, Done.
    fn sheet_spec<'a>(&self, actions: &'a [ModalAction<'a>], body_height: f64) -> ModalSpec<'a> {
        ModalSpec::new(self.sheet_title())
            .actions(actions)
            .closable(false)
            .body_height(body_height)
    }

    /// The part of the sheet's body the notes editor takes, where a press
    /// goes to the editor.
    pub(super) fn notes_region(&self) -> Rect {
        self.modal.as_ref().map_or(Rect::ZERO, |modal| {
            Rect::new(
                modal.body.x0,
                self.notes_origin.y.max(modal.body.y0),
                modal.body.x1,
                modal.body.y1,
            )
        })
    }

    /// Width of the sheet's body (the form and the notes column) in the
    /// current window and theme.
    pub(super) fn detail_body_w(&self) -> f64 {
        modal_dialog::body_width(self.rect, modal_dialog::MODAL_MAX_WIDTH, &self.layout_theme)
    }

    /// Lay the sheet out for the current selection: the form at its own
    /// height, the notes below it, the modal chrome around both, and the
    /// body's scroll. Clears the layout when no editor is open.
    pub(super) fn layout_sheet(&mut self) {
        let open = self.detail_open && self.selection.is_some() && self.rect.area() > 0.0;
        if !open {
            self.modal = None;
            self.detail_rect = Rect::ZERO;
            self.form = FormRects::default();
            self.notes_origin = Point::new(-10000.0, -10000.0);
            return;
        }
        let theme = self.layout_theme.clone();
        let window = self.rect;
        let notes_min = 2.0 * f64::from(theme.tokens.metrics.bar_height);
        let (layout, form, content_h) =
            ui::text_render::with_layout_renderer(theme.text_scale, |text| {
                let width = self.detail_body_w();
                let probe = self.form_layout(Rect::new(0.0, 0.0, width, 0.0), text, &theme);
                let content_h = probe.notes_top + self.notes_content_h.max(notes_min);
                let actions = [ModalAction::primary(DONE)];
                let natural = modal_dialog::layout(
                    window,
                    &self.sheet_spec(&actions, content_h),
                    text,
                    &theme,
                );
                // A form taller than the window scrolls inside a sheet that
                // keeps the window's header row clear, and whose body ends
                // between two rows rather than through one.
                let header = f64::from(theme.tokens.metrics.header_height);
                let chrome_h = natural.card.height() - natural.body.height();
                let room = (window.height() - 2.0 * header - chrome_h).max(0.0);
                let layout = if content_h <= room {
                    natural
                } else {
                    let body_h = probe
                        .row_bottoms()
                        .into_iter()
                        .filter(|y| *y <= room)
                        .fold(0.0, f64::max);
                    let body_h = if body_h > 0.0 { body_h } else { room };
                    modal_dialog::layout(window, &self.sheet_spec(&actions, body_h), text, &theme)
                };
                let max = (content_h - layout.body.height()).max(0.0);
                let scroll = if self.shots_detail_scroll_to_end {
                    max
                } else {
                    self.detail_scroll_y.clamp(0.0, max)
                };
                let body = Rect::new(
                    layout.body.x0,
                    layout.body.y0 - scroll,
                    layout.body.x1,
                    layout.body.y1,
                );
                let form = self.form_layout(body, text, &theme);
                (layout, form, content_h)
            });
        self.detail_scroll_max = (content_h - layout.body.height()).max(0.0);
        self.detail_scroll_y = self.detail_scroll_y.clamp(0.0, self.detail_scroll_max);
        if self.shots_detail_scroll_to_end {
            self.detail_scroll_y = self.detail_scroll_max;
        }
        self.detail_rect = layout.card;
        self.notes_origin = Point::new(layout.body.x0, form.notes_top);
        self.notes_w = layout.body.width();
        self.form = form;
        self.modal = Some(layout);
        self.place_form_widgets();
    }

    /// Point the embedded inputs and dropdowns at the fresh form geometry,
    /// parking the ones the current sheet does not show.
    fn place_form_widgets(&mut self) {
        let parked = Rect::new(-10000.0, -10000.0, -9900.0, -9900.0);
        let editing_event = matches!(self.selection, Some(Sel::Event(_)));
        let window = self.rect;
        let f = self.form.clone();
        if editing_event {
            self.title_input.apply_layout_external(f.title);
            self.location_input.apply_layout_external(f.location);
            self.invite_input.apply_layout_external(f.invite);
        } else {
            self.title_input.apply_layout_external(parked);
            self.location_input.apply_layout_external(parked);
            self.invite_input.apply_layout_external(parked);
        }
        let c = &mut self.choices;
        for (choice, rect) in [
            (&mut c.calendar, f.calendar),
            (&mut c.repeat, f.repeat),
            (&mut c.alert, f.alert),
            (&mut c.timezone, f.timezone),
            (&mut c.priority, f.priority),
            (&mut c.project, f.project),
        ] {
            choice.place(if rect.area() > 0.0 { rect } else { parked }, window);
        }
    }

    /// Seed the dropdowns from the selected event or task.
    pub(super) fn seed_choices(&mut self) {
        match self.selection {
            Some(Sel::Event(id)) if self.detail_open => {
                let Some(event) = self.model.event(id).cloned() else {
                    return;
                };
                let calendars: Vec<(String, String)> = self
                    .calendar_options()
                    .into_iter()
                    .map(|(target, label)| {
                        let value = match target {
                            CalTarget::Local => "local".to_string(),
                            CalTarget::Remote(id) => id,
                        };
                        (value, label)
                    })
                    .collect();
                let current = event.calendar_id.clone().unwrap_or_else(|| "local".into());
                let c = &mut self.choices;
                c.calendar.seed(calendars, &current);
                c.repeat.seed(repeat_options(&event), repeat_value(&event));
                c.alert.seed(alert_options(&event), &alert_value(&event));
                c.timezone.seed(timezone_options(&event), &event.timezone);
            }
            Some(Sel::Task(id)) if self.detail_open => {
                let Some(task) = self.model.task(id).cloned() else {
                    return;
                };
                let projects = std::iter::once((String::new(), "No project".to_string()))
                    .chain(
                        self.model
                            .projects
                            .iter()
                            .map(|project| (project.id.to_string(), project.name.clone())),
                    )
                    .collect();
                let project = task.project.map(|id| id.to_string()).unwrap_or_default();
                let c = &mut self.choices;
                c.priority
                    .seed(priority_options(), priority_value(task.priority));
                c.project.seed(projects, &project);
            }
            _ => {}
        }
    }

    /// Apply the picks the dropdowns published since the last pass.
    pub(super) fn commit_choices(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        path: &automerge::ObjId,
    ) {
        match self.selection {
            Some(Sel::Event(_)) => {
                if let Some(value) = self.choices.calendar.take_pick(doc, path) {
                    let target = if value == "local" {
                        CalTarget::Local
                    } else {
                        CalTarget::Remote(value)
                    };
                    self.set_event_calendar(&target, doc);
                }
                if let Some(value) = self.choices.repeat.take_pick(doc, path) {
                    self.cycle_event(doc, |e| apply_repeat(e, &value));
                }
                if let Some(value) = self.choices.alert.take_pick(doc, path) {
                    let minutes = value.parse::<i64>().ok();
                    self.cycle_event(doc, |e| e.reminder_min = minutes);
                }
                if let Some(value) = self.choices.timezone.take_pick(doc, path) {
                    self.cycle_event(doc, |e| e.timezone.clone_from(&value));
                }
            }
            Some(Sel::Task(_)) => {
                if let Some(value) = self.choices.priority.take_pick(doc, path) {
                    let priority = match value.as_str() {
                        "p1" => Priority::High,
                        "p2" => Priority::Medium,
                        "p3" => Priority::Low,
                        _ => Priority::None,
                    };
                    self.set_selected_task_priority(priority, doc);
                }
                if let Some(value) = self.choices.project.take_pick(doc, path) {
                    let project = self
                        .model
                        .projects
                        .iter()
                        .find(|project| project.id.to_string() == value)
                        .map(|project| project.id);
                    self.set_selected_task_project(project, doc);
                }
            }
            None => {}
        }
    }

    /// The widest leading label of `labels` plus the gap before its field.
    fn label_column(labels: &[&str], text: &mut TextRenderer, theme: &Theme) -> f64 {
        let style = theme.text(TextRole::Body);
        labels
            .iter()
            .map(|label| f64::from(text.measure_style(label, None, &style).0))
            .fold(0.0, f64::max)
            + f64::from(theme.tokens.spacing.s4)
    }

    /// One swatch per calendar colour along the Colour row, each centred
    /// in a pointer target, the first target flush with the field's edge.
    fn swatch_layout(row: Rect, theme: &Theme) -> Vec<(Palette, Rect)> {
        let side = f64::from(theme.tokens.metrics.icon_lg);
        let target = f64::from(theme.tokens.metrics.min_pointer_target).max(side);
        let mid = row.center().y;
        Palette::ALL
            .iter()
            .enumerate()
            .map(|(i, palette)| {
                let x0 = row.x0 + target * i as f64 + (target - side) / 2.0;
                (
                    *palette,
                    Rect::new(x0, mid - side / 2.0, x0 + side, mid + side / 2.0),
                )
            })
            .collect()
    }

    /// The form's geometry inside `body` (whose top is already scrolled).
    fn form_layout(&self, body: Rect, text: &mut TextRenderer, theme: &Theme) -> FormRects {
        match self.selection {
            Some(Sel::Event(id)) => self
                .model
                .event(id)
                .map(|event| self.event_form_layout(body, event, text, theme))
                .unwrap_or_default(),
            Some(Sel::Task(id)) => self
                .model
                .task(id)
                .map(|task| self.task_form_layout(body, task, text, theme))
                .unwrap_or_default(),
            None => FormRects::default(),
        }
    }

    fn event_form_layout(
        &self,
        body: Rect,
        event: &Event,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> FormRects {
        let sp = &theme.tokens.spacing;
        let row_h = ControlSize::Regular.height(theme);
        let mut f = FormRects::default();
        let date_w = |text: &mut TextRenderer, d: NaiveDate| {
            ButtonFace::new(Rect::ZERO, &date::fmt_day_long(d)).intrinsic_width(text, theme)
        };
        let time_w = |text: &mut TextRenderer| {
            ButtonFace::new(Rect::ZERO, "00:00").intrinsic_width(text, theme)
        };
        let gap = f64::from(sp.rhythm.related);
        let widest = date_w(text, event.start.date()).max(date_w(text, event.end.date()))
            + gap
            + time_w(text);
        let label_column = Self::label_column(&EVENT_LABELS, text, theme);
        let mut flow = Flow::new(theme, body, label_column, widest, text);
        let title_h = ControlSize::Large.height(theme);
        f.title = Rect::new(body.x0, flow.y, body.x1, flow.y + title_h);
        flow.y = f.title.y1 + f64::from(sp.rhythm.section);

        for (label, day, timed) in [
            ("Starts", event.start.date(), true),
            (
                "Ends",
                if event.all_day {
                    event.all_day_last_date()
                } else {
                    event.end.date()
                },
                true,
            ),
        ] {
            let field = flow.row(label, row_h);
            let date_rect = flow.control(field.x0, field.y0, date_w(text, day));
            let time_rect = if timed && !event.all_day {
                flow.control(date_rect.x1 + gap, field.y0, time_w(text))
            } else {
                Rect::ZERO
            };
            if label == "Starts" {
                f.start_date = date_rect;
                f.start_time = time_rect;
            } else {
                f.end_date = date_rect;
                f.end_time = time_rect;
            }
        }
        let field = flow.row("All day", row_h);
        f.all_day = Rect::new(
            field.x0,
            field.y0,
            field.x0 + 2.0 * f64::from(theme.tokens.metrics.icon_lg),
            field.y1,
        );
        if !self.local_schedule_conflicts(event).is_empty() {
            let line = flow.label_h.max(f64::from(theme.tokens.metrics.icon_md));
            let height = if flow.stacked {
                2.0 * line
            } else {
                row_h.max(line)
            };
            f.conflict = Rect::new(flow.field_x, flow.y, body.x1, flow.y + height);
            flow.y = f.conflict.y1 + gap;
        }
        flow.gap();
        f.location = flow.row("Location", row_h);
        f.calendar = flow.row("Calendar", row_h);
        f.repeat = flow.row("Repeat", row_h);
        f.alert = flow.row("Alert", row_h);
        f.timezone = flow.row("Time zone", row_h);
        f.colour = flow.row("Colour", row_h);
        f.swatches = Self::swatch_layout(f.colour, theme);
        flow.gap();
        f.invite = flow.row("Guests", row_h);
        // A guest row's avatar lines up with the Guests field above it: the
        // row reaches left past the field by the row's own inset. It ends
        // at the body's edge, where the sheet clips, so its status reads
        // whole.
        let row_inset = f64::from(sp.s3);
        for attendee in &event.attendees {
            let face = ListRowFace::new(&attendee.email).secondary(&attendee.email);
            let height = face.height(text, theme);
            let rect = Rect::new(flow.field_x - row_inset, flow.y, body.x1, flow.y + height);
            f.guests.push(rect);
            flow.y = rect.y1;
        }
        flow.gap();
        f.labels = flow.labels;
        Self::notes_heading(&mut f, body, flow.y, text, theme);
        f
    }

    fn task_form_layout(
        &self,
        body: Rect,
        task: &Task,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> FormRects {
        let sp = &theme.tokens.spacing;
        let row_h = ControlSize::Regular.height(theme);
        let mut f = FormRects::default();
        let due_label = task
            .due
            .map_or_else(|| "Add a due date".to_string(), date::fmt_day_long);
        let due_w = ButtonFace::new(Rect::ZERO, &due_label).intrinsic_width(text, theme);
        let label_column = Self::label_column(&TASK_LABELS, text, theme);
        let mut flow = Flow::new(theme, body, label_column, due_w, text);
        // The checkbox and the title share the first line.
        let side = f64::from(theme.tokens.metrics.icon_md);
        let title_style = theme.text(TextRole::Heading);
        let check_x1 = body.x0 + side + f64::from(theme.tokens.metrics.icon_label_gap);
        let (_, title_h) = text.measure_style(
            &task.title,
            Some(ui::num::f32_of_f64((body.x1 - check_x1).max(0.0))),
            &title_style,
        );
        let line = f64::from(title_style.line_height_px());
        f.task_check = Rect::new(
            body.x0,
            flow.y + (line - side) / 2.0,
            body.x0 + side,
            flow.y + f64::midpoint(line, side),
        );
        f.task_title = Rect::new(check_x1, flow.y, body.x1, flow.y + f64::from(title_h));
        flow.y = f.task_title.y1 + f64::from(sp.rhythm.section);

        let due = flow.row("Due", row_h);
        f.due = flow.control(due.x0, due.y0, due_w);
        f.priority = flow.row("Priority", row_h);
        f.project = flow.row("Project", row_h);
        let subtasks = self.model.subtasks_of(task.id);
        if subtasks.is_empty() {
            f.labels = std::mem::take(&mut flow.labels);
        } else {
            flow.gap();
            let heading = theme.text(TextRole::Subheading);
            let heading_h = f64::from(heading.line_height_px());
            f.labels = std::mem::take(&mut flow.labels);
            f.labels.push((
                "Subtasks",
                Rect::new(body.x0, flow.y, body.x1, flow.y + heading_h),
            ));
            flow.y += heading_h + f64::from(sp.rhythm.related);
            for _ in subtasks {
                let rect = Rect::new(body.x0, flow.y, body.x1, flow.y + row_h);
                f.subtasks.push(rect);
                flow.y = rect.y1;
            }
        }
        flow.gap();
        Self::notes_heading(&mut f, body, flow.y, text, theme);
        f
    }

    fn notes_heading(
        f: &mut FormRects,
        body: Rect,
        y: f64,
        _text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let style = theme.text(TextRole::Subheading);
        let h = f64::from(style.line_height_px());
        f.notes_label = Rect::new(body.x0, y, body.x1, y + h);
        f.notes_top = f.notes_label.y1 + f64::from(theme.tokens.spacing.rhythm.related);
    }

    /// Paint the sheet in the top layer and append its hit targets.
    pub(super) fn draw_sheet(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(layout) = self.modal.clone() else {
            return;
        };
        let actions = [ModalAction::primary(DONE).hovered(
            layout
                .actions
                .first()
                .is_some_and(|r| r.contains(self.last_mouse)),
        )];
        let spec = self.sheet_spec(&actions, layout.body.height());
        modal_dialog::paint_backdrop(scene, t, text, theme, &layout, &spec);
        self.hits.push((layout.scrim, Action::CloseDetail));
        self.hits.push((layout.card, Action::Noop));

        let sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &layout.body,
        );
        let mut hits = Vec::new();
        match self.selection {
            Some(Sel::Event(id)) => {
                if let Some(event) = self.model.event(id).cloned() {
                    self.draw_event_form(
                        &mut PaintCtx::new(scene, t, text, &sheet),
                        presence,
                        &event,
                        &mut hits,
                    );
                }
            }
            Some(Sel::Task(id)) => {
                if let Some(task) = self.model.task(id).cloned() {
                    self.draw_task_form(
                        &mut PaintCtx::new(scene, t, text, &sheet),
                        &task,
                        &mut hits,
                    );
                    for choice in self.choices.shown_mut(true) {
                        Widget::draw(&mut choice.dropdown, scene, t, text, &sheet, presence);
                    }
                }
            }
            None => {}
        }
        let mut ctx = PaintCtx::new(scene, t, text, &sheet);
        text_in_band(
            &mut ctx,
            "Notes",
            &sheet.text(TextRole::Subheading),
            sheet.text_primary,
            self.form.notes_label.x0,
            self.form.notes_label,
            self.form.notes_label.width(),
        );
        self.notes_editor.draw(scene, t, text, &sheet, presence);
        scene.pop_layer();
        if self.detail_scroll_max > 0.0 {
            // The body scrolls: the shared quiet thumb in the sheet's
            // trailing padding says how much more there is.
            let gap = f64::from(sheet.tokens.spacing.s2);
            let track = Rect::new(
                layout.body.x1 + gap,
                layout.body.y0,
                layout.body.x1 + 2.0 * gap,
                layout.body.y1,
            );
            ui::widget::Scrollbar::vertical().draw(
                scene,
                t,
                track,
                ui::widget::scrollbar::ScrollMetrics::new(
                    layout.body.height() + self.detail_scroll_max,
                    layout.body.height(),
                    self.detail_scroll_y,
                ),
                &sheet,
                false,
            );
        }
        // Only the visible part of the body is a target.
        hits.retain(|(rect, _)| rect.intersect(layout.body).area() > 0.0);
        self.hits.extend(hits);
        self.draw_sheet_footer(scene, t, text, theme, &layout, &spec);
        self.notes_editor
            .draw_overlay(scene, t, text, &sheet, presence);
        self.draw_sheet_pickers(scene, t, text, theme, presence);
    }

    /// The sheet's footer: the destructive action leads, Done trails.
    fn draw_sheet_footer(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        layout: &ModalLayout,
        spec: &ModalSpec<'_>,
    ) {
        let delete = match self.selection {
            // An event that was never saved is discarded, not deleted.
            Some(Sel::Event(id)) if self.fresh_draft.is_some() => {
                Some(("Discard", Action::DiscardDraft(id)))
            }
            Some(Sel::Event(id)) => Some(("Delete event", Action::DeleteEvent(id))),
            Some(Sel::Task(id)) => Some(("Delete task", Action::DeleteTask(id))),
            None => None,
        };
        if let (Some((label, action)), Some(done)) = (delete, layout.actions.first()) {
            let face = ButtonFace::new(Rect::ZERO, label).role(ButtonRole::Destructive);
            let width = face.intrinsic_width(text, theme);
            let rect = Rect::new(layout.body.x0, done.y0, layout.body.x0 + width, done.y1);
            Button::paint(
                scene,
                t,
                text,
                theme,
                &ButtonFace::new(rect, label)
                    .role(ButtonRole::Destructive)
                    .hovered(rect.contains(self.last_mouse)),
            );
            self.hits.push((rect, action));
        }
        modal_dialog::paint_actions(scene, t, text, theme, layout, spec);
        if let Some(done) = layout.actions.first() {
            self.hits.push((*done, Action::CloseDetail));
        }
    }

    /// Open pickers float over the sheet.
    fn draw_sheet_pickers(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let sheet = theme.on_ground(ui::chrome::panel_ground(theme));
        for choice in self
            .choices
            .shown_mut(matches!(self.selection, Some(Sel::Task(_))))
        {
            if choice.dropdown.is_open() {
                Widget::draw_overlay(&mut choice.dropdown, scene, t, text, &sheet, presence);
            }
        }
        if matches!(self.selection, Some(Sel::Event(_))) {
            self.event_popovers.draw(
                &mut PaintCtx::new(scene, t, text, theme),
                presence,
                PopoverAnchors {
                    start_date: self.form.start_date,
                    end_date: self.form.end_date,
                    start_time: self.form.start_time,
                    end_time: self.form.end_time,
                    panel: self.rect,
                },
            );
        }
        if self.task_popover == Some(TaskPopover::Due) {
            self.draw_task_due_popover(scene, t, text, theme);
        }
    }

    fn draw_event_form(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        presence: &PresenceCtx,
        event: &Event,
        hits: &mut Vec<(Rect, Action)>,
    ) {
        let (scene, t, text, sheet) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let f = self.form.clone();
        let mouse = self.last_mouse;
        Self::draw_form_labels(&mut PaintCtx::new(scene, t, text, sheet), &f);

        self.title_input.draw(scene, t, text, sheet, presence);
        hits.push((f.title, Action::FocusField(FormField::Title)));

        let popover = self.event_popovers.active();
        for (rect, label, kind) in [
            (
                f.start_date,
                date::fmt_day_long(event.start.date()),
                FormPopover::StartDate,
            ),
            (
                f.start_time,
                date::fmt_time(&event.start),
                FormPopover::StartTime,
            ),
            (
                f.end_date,
                date::fmt_day_long(if event.all_day {
                    event.all_day_last_date()
                } else {
                    event.end.date()
                }),
                FormPopover::EndDate,
            ),
            (f.end_time, date::fmt_time(&event.end), FormPopover::EndTime),
        ] {
            if rect.area() <= 0.0 {
                continue;
            }
            Button::paint(
                scene,
                t,
                text,
                sheet,
                &ButtonFace::new(rect, &label)
                    .hovered(rect.contains(mouse))
                    .pressed(popover == kind),
            );
            hits.push((rect, Action::OpenPopover(kind)));
        }

        Toggle::paint(
            scene,
            t,
            sheet,
            &ToggleFace::new(f.all_day, event.all_day)
                .pose(
                    self.faces
                        .switch(("all-day", event.id), event.all_day, sheet.motion()),
                )
                .hovered(f.all_day.contains(mouse)),
        );
        hits.push((f.all_day, Action::ToggleAllDay));

        if f.conflict.area() > 0.0 {
            let count = self.local_schedule_conflicts(event).len();
            Self::draw_conflict(&mut PaintCtx::new(scene, t, text, sheet), f.conflict, count);
        }

        self.location_input.draw(scene, t, text, sheet, presence);
        hits.push((f.location, Action::FocusField(FormField::Location)));

        for choice in self.choices.shown_mut(false) {
            Widget::draw(&mut choice.dropdown, scene, t, text, sheet, presence);
        }

        // The event's colour is user content: the swatches show the colours
        // themselves, the chosen one ringed.
        let target_pad = (f64::from(sheet.tokens.metrics.min_pointer_target)
            - f64::from(sheet.tokens.metrics.icon_lg))
        .max(0.0)
            / 2.0;
        for (palette, rect) in &f.swatches {
            let target = rect.inflate(target_pad, target_pad);
            ColorSwatchFace::new(*rect, palette.color(sheet), palette.name())
                .selected(event.color == *palette)
                .hovered(target.contains(mouse))
                .target_rect(target)
                .paint(scene, t, text, sheet);
            hits.push((target, Action::SetEventColor(*palette)));
        }

        self.invite_input.draw(scene, t, text, sheet, presence);
        hits.push((f.invite, Action::FocusField(FormField::Invite)));
        let mut ctx = PaintCtx::new(scene, t, text, sheet);
        for (attendee, rect) in event.attendees.iter().zip(&f.guests) {
            let name = if attendee.name.is_empty() {
                attendee.email.as_str()
            } else {
                attendee.name.as_str()
            };
            let organizer = event.organizer == Some(attendee.id);
            let status = if organizer {
                "Organizer"
            } else {
                attendee.status.label()
            };
            ListRowFace::new(name)
                .leading(RowLeading::Avatar(AvatarFace::new(name, &attendee.email)))
                .secondary(&attendee.email)
                .meta(status)
                .paint(&mut ctx, *rect);
        }
    }

    /// The local schedule overlap: a warning mark and one sentence.
    fn draw_conflict(ctx: &mut PaintCtx<'_>, rect: Rect, count: usize) {
        let sheet = ctx.theme;
        let label = if count == 1 {
            "Overlaps another saved event".to_string()
        } else {
            format!("Overlaps {count} saved events")
        };
        let side = f64::from(sheet.tokens.metrics.icon_md);
        let mid = rect.center().y;
        Icon::TriangleAlert.draw(
            ctx.scene,
            ctx.transform,
            Rect::new(rect.x0, mid - side / 2.0, rect.x0 + side, mid + side / 2.0),
            sheet.tokens.colors.semantic.warning,
        );
        let x = rect.x0 + side + f64::from(sheet.tokens.metrics.icon_label_gap);
        text_in_band(
            ctx,
            &label,
            &sheet.text(TextRole::Body),
            sheet.text_primary,
            x,
            rect,
            rect.x1 - x,
        );
    }

    fn draw_form_labels(ctx: &mut PaintCtx<'_>, f: &FormRects) {
        let theme = ctx.theme;
        let body = theme.text(TextRole::Body);
        let subheading = theme.text(TextRole::Subheading);
        for (label, band) in &f.labels {
            let (style, ink) = if *label == "Subtasks" {
                (&subheading, theme.text_primary)
            } else {
                (&body, theme.text_dim)
            };
            text_in_band(ctx, label, style, ink, band.x0, *band, band.width());
        }
    }

    fn draw_task_form(&self, ctx: &mut PaintCtx<'_>, task: &Task, hits: &mut Vec<(Rect, Action)>) {
        let theme = ctx.theme;
        let f = self.form.clone();
        let mouse = self.last_mouse;
        Checkbox::paint(
            ctx.scene,
            ctx.transform,
            theme,
            &CheckboxFace::new(f.task_check, task.done)
                .pose(self.faces.check(
                    ("done", task.id),
                    task.done,
                    CheckGlyph::Check,
                    theme.motion(),
                ))
                .hovered(f.task_check.contains(mouse)),
        );
        hits.push((f.task_check, Action::ToggleSelectedTask));
        let ink = if task.done {
            theme.text_dim
        } else {
            theme.text_primary
        };
        ctx.text(
            &task.title,
            &theme.text(TextRole::Heading),
            ink,
            Point::new(f.task_title.x0, f.task_title.y0),
            Some(ui::num::f32_of_f64(f.task_title.width())),
        );
        Self::draw_form_labels(ctx, &f);

        let due = task
            .due
            .map_or_else(|| "Add a due date".to_string(), date::fmt_day_long);
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            theme,
            &ButtonFace::new(f.due, &due)
                .hovered(f.due.contains(mouse))
                .pressed(self.task_popover == Some(TaskPopover::Due)),
        );
        hits.push((f.due, Action::OpenTaskPopover(TaskPopover::Due)));

        let side = f64::from(theme.tokens.metrics.icon_md);
        let gap = f64::from(theme.tokens.metrics.icon_label_gap);
        for (subtask, rect) in self.model.subtasks_of(task.id).iter().zip(&f.subtasks) {
            let mid = rect.center().y;
            let check = Rect::new(rect.x0, mid - side / 2.0, rect.x0 + side, mid + side / 2.0);
            Checkbox::paint(
                ctx.scene,
                ctx.transform,
                theme,
                &CheckboxFace::new(check, subtask.done)
                    .pose(self.faces.check(
                        ("done", subtask.id),
                        subtask.done,
                        CheckGlyph::Check,
                        theme.motion(),
                    ))
                    .hovered(rect.contains(mouse)),
            );
            let ink = if subtask.done {
                theme.text_dim
            } else {
                theme.text_primary
            };
            text_in_band(
                ctx,
                &subtask.title,
                &theme.text(TextRole::Body),
                ink,
                check.x1 + gap,
                *rect,
                rect.x1 - check.x1 - gap,
            );
            hits.push((*rect, Action::ToggleTask(subtask.id)));
        }
    }

    /// The date grid under the task's Due button, with a Clear action.
    fn draw_task_due_popover(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let sp = &theme.tokens.spacing;
        let m = &theme.tokens.metrics;
        let pad = f64::from(sp.s3);
        let width = 7.0 * f64::from(m.min_pointer_target);
        let grid_h =
            f64::from(m.min_pointer_target + m.icon_md) + 6.0 * f64::from(m.min_pointer_target);
        let clear_h = ControlSize::Regular.height(theme);
        let height = grid_h + f64::from(sp.rhythm.related) + clear_h;
        let anchor = self.form.due;
        let x0 = anchor.x0.min(self.rect.x1 - width - 2.0 * pad);
        let y0 = (anchor.y1 + f64::from(sp.s1)).min(self.rect.y1 - height - 2.0 * pad);
        let panel = Rect::new(x0, y0, x0 + width + 2.0 * pad, y0 + height + 2.0 * pad);
        ui::widget::floating_surface::paint(scene, t, panel, theme);
        self.hits.push((self.rect, Action::DismissTaskPopover));
        self.hits.push((panel, Action::Noop));
        let on_panel = theme.on_ground(ui::chrome::panel_ground(theme));
        let picker = Rect::new(
            panel.x0 + pad,
            panel.y0 + pad,
            panel.x1 - pad,
            panel.y0 + pad + grid_h,
        );
        self.task_due_picker.apply_layout_external(picker);
        Widget::draw(
            &mut self.task_due_picker,
            scene,
            t,
            text,
            &on_panel,
            &PresenceCtx::empty(),
        );
        for (hit, rect) in self.task_due_picker.hit_regions_external() {
            let action = match hit {
                ui::widget::ExternalDatePickerHit::PreviousMonth => Action::ShiftTaskDueMonth(-1),
                ui::widget::ExternalDatePickerHit::NextMonth => Action::ShiftTaskDueMonth(1),
                ui::widget::ExternalDatePickerHit::Date(day) => Action::SetTaskDue(Some(day)),
            };
            self.hits.push((rect, action));
        }
        let clear = Rect::new(
            picker.x0,
            panel.y1 - pad - clear_h,
            picker.x1,
            panel.y1 - pad,
        );
        Button::paint(
            scene,
            t,
            text,
            &on_panel,
            &ButtonFace::new(clear, "Clear due date").hovered(clear.contains(self.last_mouse)),
        );
        self.hits.push((clear, Action::SetTaskDue(None)));
    }

    /// The task the sheet shows the due date of, a day from now when it has
    /// none, so the picker opens somewhere useful.
    pub(super) fn task_due_seed(&self) -> NaiveDate {
        self.selected_task()
            .and_then(|id| self.model.task(id))
            .and_then(|task| task.due)
            .unwrap_or(self.anchor)
    }
}
