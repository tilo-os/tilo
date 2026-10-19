//! The calendar's items as JSON (what its verbs take: an event or a task
//! as a server or a file gives it), the ids its items are published under,
//! and its calendar collections as live rows.
//!
//! The calendar itself — its events, tasks, projects and notes, and what
//! each server last confirmed — is the [`Calendar`] record; its fields
//! change through field edits at their paths and its verbs
//! ([`crate::resource`]).

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Effect as _, JsonSchema, items::item_resource_id};

use crate::model::{
    Attendee, Calendar, Event, Id, InviteStatus, OccurrenceOverride, Palette, Priority, Recurrence,
    Task, key,
};

/// The published id of the event `id`: an item of the calendar's events.
pub fn event_resource_id(id: Id) -> String {
    item_resource_id::<Calendar>("events", &key(id))
}

/// The published id of the task `id`.
pub fn task_resource_id(id: Id) -> String {
    item_resource_id::<Calendar>("tasks", &key(id))
}

/// The published id of the task project `id`.
pub fn project_resource_id(id: Id) -> String {
    item_resource_id::<Calendar>("projects", &key(id))
}

/// The id of a local or configured remote calendar collection's row.
pub fn calendar_resource_id(id: &str) -> String {
    format!("calendar.collection:{id}")
}

/// The row of the attendee `attendee_id` of the event `event_id`.
pub fn attendee_resource_id(event_id: Id, attendee_id: Id) -> String {
    format!("{}/attendees/{attendee_id}", event_resource_id(event_id))
}

/// What opens the occurrence of the event `event_id` originally on
/// `original_date`: the event's id and the date.
pub fn occurrence_resource_id(event_id: Id, original_date: NaiveDate) -> String {
    format!(
        "{}/occurrences/{}",
        event_resource_id(event_id),
        original_date.format(DATE_FMT)
    )
}

/// A calendar collection as a live row: open it, show or hide it, choose
/// its colour.
pub(crate) fn calendar_collection_row(
    calendar: &CalendarCollectionResource,
) -> ui::framework::LiveRow {
    ui::framework::LiveRow::new(
        calendar_resource_id(&calendar.id),
        "calendar.collection",
        calendar.label.clone(),
    )
    .effect(&crate::resource::CalendarEffect::Open {
        item: calendar_resource_id(&calendar.id),
    })
    .effect(&crate::resource::CalendarEffect::SetVisibility {
        id: calendar.id.clone(),
        visible: !calendar.visible,
    })
    .operation(
        crate::resource::CalendarEffect::SetColor {
            id: calendar.id.clone(),
            color: Palette::from_index(calendar.color.unwrap_or_default())
                .name()
                .to_ascii_lowercase(),
        }
        .bound_asking(&["color"]),
    )
}

/// How an event's start and end are written in its JSON form.
pub(crate) const DT_FMT: &str = "%Y-%m-%dT%H:%M";
/// How dates are written in the JSON forms.
pub(crate) const DATE_FMT: &str = "%Y-%m-%d";

/// An event as JSON, as a verb takes one: datetimes as `YYYY-MM-DDTHH:MM`,
/// colour as a palette index, recurrence as an RRULE body, no reminder as
/// `-1`. [`Self::from_event`] and [`Self::to_event`] round-trip losslessly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EventData {
    pub id: String,
    pub title: String,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub end_later_fold: bool,
    pub all_day: bool,
    pub location: String,
    pub color: usize,
    pub rrule: String,
    #[serde(default = "default_recurrence_interval")]
    pub recurrence_interval: u32,
    #[serde(default)]
    pub recurrence_until: String,
    #[serde(default)]
    pub recurrence_count: u32,
    #[serde(default)]
    pub recurrence_exdates: Vec<String>,
    #[serde(default)]
    pub occurrence_overrides: Vec<OccurrenceOverrideData>,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub organizer: String,
    #[serde(default)]
    pub attendees: Vec<AttendeeData>,
    /// Minutes before start for a reminder, or `-1` for none.
    pub reminder: i64,
    /// Owning project id (string form), or `""` for none.
    pub project: String,
    /// Source calendar id, or `""` for the local on-device calendar.
    pub calendar_id: String,
}

fn default_recurrence_interval() -> u32 {
    1
}

fn default_timezone() -> String {
    "floating".into()
}

/// An event left to fill in: no times yet, on the local calendar, not
/// repeating, no reminder.
impl Default for EventData {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            start: String::new(),
            end: String::new(),
            end_later_fold: false,
            all_day: false,
            location: String::new(),
            color: 0,
            rrule: String::new(),
            recurrence_interval: default_recurrence_interval(),
            recurrence_until: String::new(),
            recurrence_count: 0,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: default_timezone(),
            organizer: String::new(),
            attendees: Vec::new(),
            reminder: -1,
            project: String::new(),
            calendar_id: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttendeeData {
    pub id: String,
    pub name: String,
    pub email: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OccurrenceOverrideData {
    pub original_date: String,
    #[serde(default)]
    pub original_time: String,
    pub cancelled: bool,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub end_later_fold: bool,
    pub title: String,
    pub location: String,
}

impl EventData {
    pub fn from_event(e: &Event) -> Self {
        Self {
            id: e.id.to_string(),
            title: e.title.clone(),
            start: e.start.format(DT_FMT).to_string(),
            end: e.end.format(DT_FMT).to_string(),
            end_later_fold: e.end_later_fold,
            all_day: e.all_day,
            location: e.location.clone(),
            color: e.color.index(),
            rrule: e.recurrence.to_rrule(),
            recurrence_interval: e.recurrence_interval,
            recurrence_until: e
                .recurrence_until
                .map_or_else(String::new, |date| date.to_string()),
            recurrence_count: e.recurrence_count.unwrap_or(0),
            recurrence_exdates: e
                .recurrence_exdates
                .iter()
                .map(ToString::to_string)
                .collect(),
            occurrence_overrides: e
                .occurrence_overrides
                .iter()
                .map(OccurrenceOverrideData::from_override)
                .collect(),
            timezone: e.timezone.clone(),
            organizer: e.organizer.map_or_else(String::new, |id| id.to_string()),
            attendees: e
                .attendees
                .iter()
                .map(AttendeeData::from_attendee)
                .collect(),
            // External imports may carry an alarm interval Calendar does not
            // offer in its closed editing domain. Normalize at the action
            // authoring boundary so replay never persists an unsupported
            // scalar while preserving the event itself.
            reminder: e
                .reminder_min
                .filter(|minutes| matches!(*minutes, 10 | 30 | 60))
                .unwrap_or(-1),
            project: e.project.map(|p| p.to_string()).unwrap_or_default(),
            calendar_id: e.calendar_id.clone().unwrap_or_default(),
        }
    }

    /// Reconstruct the model event, or `None` when the payload is malformed (an
    /// unparseable id or datetime) so `apply` can no-op instead of substituting a
    /// wall-clock value, which would break replay determinism.
    pub fn to_event(&self) -> Option<Event> {
        let id = self.id.parse::<Id>().ok()?;
        let mut start = NaiveDateTime::parse_from_str(&self.start, DT_FMT).ok()?;
        let mut end = NaiveDateTime::parse_from_str(&self.end, DT_FMT).ok()?;
        let timezone = normalize_timezone(&self.timezone)?;
        if timezone != "floating" {
            start = crate::timezone::normalize_wall(&timezone, start)?.0;
            end = crate::timezone::normalize_wall(&timezone, end)?.0;
        }
        let attendees = self
            .attendees
            .iter()
            .map(AttendeeData::to_attendee)
            .collect::<Option<Vec<_>>>()?;
        let organizer = parse_optional_id(&self.organizer)?;
        if !crate::timezone::valid_span(&timezone, start, end, self.end_later_fold)
            || self.color >= Palette::ALL.len()
            || !matches!(self.reminder, -1 | 10 | 30 | 60)
            || Recurrence::from_rrule(&self.rrule).to_rrule() != self.rrule
            || (!self.project.is_empty() && self.project.parse::<Id>().is_err())
            || organizer
                .is_some_and(|organizer| !attendees.iter().any(|attendee| attendee.id == organizer))
        {
            return None;
        }
        Some(Event {
            id,
            title: self.title.clone(),
            start,
            end,
            end_later_fold: self.end_later_fold,
            all_day: self.all_day,
            location: self.location.clone(),
            color: Palette::from_index(self.color),
            recurrence: Recurrence::from_rrule(&self.rrule),
            recurrence_interval: self.recurrence_interval.max(1),
            recurrence_until: parse_optional_date(&self.recurrence_until)?,
            recurrence_count: (self.recurrence_count != 0).then_some(self.recurrence_count),
            recurrence_exdates: self
                .recurrence_exdates
                .iter()
                .map(|date| NaiveDate::parse_from_str(date, DATE_FMT))
                .collect::<Result<_, _>>()
                .ok()?,
            occurrence_overrides: self
                .occurrence_overrides
                .iter()
                .map(OccurrenceOverrideData::to_override)
                .collect::<Option<Vec<_>>>()?
                .into(),
            timezone,
            organizer,
            attendees: attendees.into(),
            reminder_min: (self.reminder != -1).then_some(self.reminder),
            project: parse_opt_id(&self.project),
            calendar_id: if self.calendar_id.is_empty() {
                None
            } else {
                Some(self.calendar_id.clone())
            },
        })
    }
}

#[allow(
    clippy::option_option,
    reason = "outer option rejects malformed input; inner option represents an explicit empty value"
)]
fn parse_optional_date(value: &str) -> Option<Option<NaiveDate>> {
    if value.is_empty() {
        Some(None)
    } else {
        NaiveDate::parse_from_str(value, DATE_FMT).ok().map(Some)
    }
}

#[allow(
    clippy::option_option,
    reason = "outer option rejects malformed input; inner option represents an explicit empty value"
)]
fn parse_optional_id(value: &str) -> Option<Option<Id>> {
    if value.is_empty() {
        Some(None)
    } else {
        value.parse::<Id>().ok().map(Some)
    }
}

pub(crate) fn normalize_timezone(value: &str) -> Option<String> {
    (value.len() <= 128)
        .then(|| crate::timezone::canonical_zone(value))
        .flatten()
}

impl AttendeeData {
    fn from_attendee(value: &Attendee) -> Self {
        Self {
            id: value.id.to_string(),
            name: value.name.clone(),
            email: value.email.clone(),
            status: value.status.as_ical().into(),
        }
    }

    fn to_attendee(&self) -> Option<Attendee> {
        let email = self.email.trim();
        Some(Attendee {
            id: self.id.parse().ok()?,
            name: self.name.trim().to_string(),
            email: email_address(email)?.to_string(),
            status: InviteStatus::from_ical(&self.status),
        })
    }
}

pub(crate) fn email_address(value: &str) -> Option<&str> {
    let (local, domain) = value.split_once('@')?;
    (!local.is_empty() && domain.contains('.') && !domain.contains(char::is_whitespace))
        .then_some(value)
}

impl OccurrenceOverrideData {
    fn from_override(value: &OccurrenceOverride) -> Self {
        Self {
            original_date: value.original_date.to_string(),
            original_time: value
                .original_time
                // format: machine (the override's stored time of day)
                .map_or_else(String::new, |time| time.format("%H:%M:%S").to_string()),
            cancelled: value.cancelled,
            start: value
                .start
                .map_or_else(String::new, |date| date.format(DT_FMT).to_string()),
            end: value
                .end
                .map_or_else(String::new, |date| date.format(DT_FMT).to_string()),
            end_later_fold: value.end_later_fold,
            title: value.title.clone().unwrap_or_default(),
            location: value.location.clone().unwrap_or_default(),
        }
    }

    fn to_override(&self) -> Option<OccurrenceOverride> {
        let start = if self.start.is_empty() {
            None
        } else {
            Some(NaiveDateTime::parse_from_str(&self.start, DT_FMT).ok()?)
        };
        let end = if self.end.is_empty() {
            None
        } else {
            Some(NaiveDateTime::parse_from_str(&self.end, DT_FMT).ok()?)
        };
        if !self.end_later_fold && start.zip(end).is_some_and(|(start, end)| start > end) {
            return None;
        }
        Some(OccurrenceOverride {
            original_date: NaiveDate::parse_from_str(&self.original_date, DATE_FMT).ok()?,
            original_time: if self.original_time.is_empty() {
                None
            } else {
                Some(NaiveTime::parse_from_str(&self.original_time, "%H:%M:%S").ok()?)
            },
            cancelled: self.cancelled,
            start,
            end,
            end_later_fold: self.end_later_fold,
            title: (!self.title.is_empty()).then(|| self.title.clone()),
            location: (!self.location.is_empty()).then(|| self.location.clone()),
        })
    }
}

/// A task as JSON, as a verb takes one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TaskData {
    pub id: String,
    pub title: String,
    pub done: bool,
    /// Due date (`DATE_FMT`), or `""` for none.
    pub due: String,
    /// Priority rank (0 = highest).
    pub priority: u8,
    pub project: String,
    pub parent: String,
}

/// A task left to fill in: not done, no date, no priority.
impl Default for TaskData {
    fn default() -> Self {
        Self {
            id: String::new(),
            title: String::new(),
            done: false,
            due: String::new(),
            priority: Priority::None.rank(),
            project: String::new(),
            parent: String::new(),
        }
    }
}

impl TaskData {
    pub fn from_task(t: &Task) -> Self {
        Self {
            id: t.id.to_string(),
            title: t.title.clone(),
            done: t.done,
            due: t
                .due
                .map(|d| d.format(DATE_FMT).to_string())
                .unwrap_or_default(),
            priority: t.priority.rank(),
            project: t.project.map(|p| p.to_string()).unwrap_or_default(),
            parent: t.parent.map(|p| p.to_string()).unwrap_or_default(),
        }
    }

    pub fn to_task(&self) -> Option<Task> {
        let id = self.id.parse::<Id>().ok()?;
        if self.priority > Priority::None.rank()
            || (!self.due.is_empty() && NaiveDate::parse_from_str(&self.due, DATE_FMT).is_err())
            || (!self.project.is_empty() && self.project.parse::<Id>().is_err())
            || (!self.parent.is_empty() && self.parent.parse::<Id>().is_err())
        {
            return None;
        }
        Some(Task {
            id,
            title: self.title.clone(),
            done: self.done,
            due: (!self.due.is_empty())
                .then(|| NaiveDate::parse_from_str(&self.due, DATE_FMT).ok())
                .flatten(),
            priority: Priority::from_rank(self.priority),
            project: parse_opt_id(&self.project),
            parent: parse_opt_id(&self.parent),
        })
    }
}

fn parse_opt_id(s: &str) -> Option<Id> {
    if s.is_empty() {
        None
    } else {
        s.parse::<Id>().ok()
    }
}

/// The events and tasks of the iCalendar `source`, retaining their UID-derived
/// identities so a repeated import also recognises locally edited items.
/// `None` when it holds neither.
pub fn import_payload(source: &str) -> Option<(Vec<EventData>, Vec<TaskData>)> {
    let (events, tasks) = crate::ics::parse(source);
    let events: Vec<_> = events
        .into_iter()
        .map(|event| EventData::from_event(&event))
        .collect();
    let tasks: Vec<_> = tasks.iter().map(TaskData::from_task).collect();
    (!events.is_empty() || !tasks.is_empty()).then_some((events, tasks))
}

/// A local or remote collection events are filed under, as the accounts
/// configured on this device list it: its stable id, label, colour and
/// whether it is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarCollectionResource {
    pub id: String,
    pub label: String,
    pub color: Option<usize>,
    pub visible: bool,
}

#[cfg(test)]
mod tests;
