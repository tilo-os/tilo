//! The calendar's items as JSON (what its verbs take: an event or a task
//! as a server or a file gives it), the ids its items are published under,
//! and its calendar collections as values.

use jiff::civil::{Date, DateTime};
use serde::{Deserialize, Serialize};
use tilo_resource::{self, JsonSchema, items::item_resource_id};

use crate::model::{
    Attendee, Calendar, Event, Id, InviteStatus, OccurrenceOverride, Palette, Priority, Recurrence,
    Task, email_address, key, normalize_timezone,
};

pub fn event_resource_id(id: Id) -> String {
    item_resource_id::<Calendar>("events", &key(id))
}

pub fn task_resource_id(id: Id) -> String {
    item_resource_id::<Calendar>("tasks", &key(id))
}

pub fn project_resource_id(id: Id) -> String {
    item_resource_id::<Calendar>("projects", &key(id))
}

pub const LOCAL_CALENDAR_ID: &str = "local";

pub fn calendar_resource_id(id: &str) -> String {
    format!("calendar.collection:{id}")
}

pub fn attendee_resource_id(event_id: Id, attendee_id: Id) -> String {
    format!("{}/attendees/{attendee_id}", event_resource_id(event_id))
}

pub fn occurrence_resource_id(event_id: Id, original_date: Date) -> String {
    format!(
        "{}/occurrences/{}",
        event_resource_id(event_id),
        tilo_formats::date_text(original_date)
    )
}

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
    pub reminder: i64,
    pub project: String,
    pub calendar_id: String,
    /// What it is filed under, in its person's own words.
    pub categories: Vec<String>,
}

fn default_recurrence_interval() -> u32 {
    1
}

fn default_timezone() -> String {
    "floating".into()
}

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
            categories: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttendeeData {
    /// Its id; one given none is named by its address.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub email: String,
    #[serde(default)]
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
            start: tilo_formats::minute_text(e.start),
            end: tilo_formats::minute_text(e.end),
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
            // An imported alarm interval Calendar cannot offer is normalised at authoring, so replay never
            // persists an unsupported scalar and the event itself is kept.
            reminder: e
                .reminder_min
                .filter(|minutes| crate::model::REMINDERS.contains(minutes))
                .unwrap_or(-1),
            project: e.project.map(|p| p.to_string()).unwrap_or_default(),
            calendar_id: e.calendar_id.clone().unwrap_or_default(),
            categories: e.categories.iter().cloned().collect(),
        }
    }

    pub fn to_event(&self) -> Option<Event> {
        self.event().ok()
    }

    /// When it is: its start and end as its zone reads them, and that zone.
    fn moments(&self) -> Result<(DateTime, DateTime, String), String> {
        const MOMENT: &str = "a date and time such as 2026-11-20T09:00";
        let start = tilo_formats::minute_from_text(&self.start)
            .ok_or_else(|| format!("start is {MOMENT}"))?;
        let end =
            tilo_formats::minute_from_text(&self.end).ok_or_else(|| format!("end is {MOMENT}"))?;
        let timezone = normalize_timezone(&self.timezone).ok_or_else(|| {
            "timezone is an IANA time zone such as Europe/Berlin, or `floating` for the same \
             time of day everywhere"
                .to_owned()
        })?;
        if timezone == "floating" {
            return Ok((start, end, timezone));
        }
        let moment = |field: &str, wall| {
            crate::timezone::normalize_wall(&timezone, wall)
                .map(|(wall, _)| wall)
                .ok_or_else(|| format!("{field} names no moment in {timezone}"))
        };
        let (start, end) = (moment("start", start)?, moment("end", end)?);
        Ok((start, end, timezone))
    }

    /// Who is invited, and which of them organizes it.
    fn guests(&self) -> Result<(Vec<Attendee>, Option<Id>), String> {
        let attendees = self
            .attendees
            .iter()
            .map(AttendeeData::to_attendee)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                "attendees each have a complete email address, such as name@example.com".to_owned()
            })?;
        let organizer = parse_optional_id(&self.organizer)
            .filter(|organizer| {
                organizer.is_none_or(|organizer| {
                    attendees.iter().any(|attendee| attendee.id == organizer)
                })
            })
            .ok_or_else(|| "organizer is the id of one of the attendees".to_owned())?;
        Ok((attendees, organizer))
    }

    /// The fields that take one of a few values, each held to them.
    fn choices(&self) -> Result<(), String> {
        if self.color >= Palette::ALL.len() {
            return Err(format!(
                "color is a number from 0 to {}",
                Palette::ALL.len().saturating_sub(1)
            ));
        }
        if self.reminder != -1 && !crate::model::REMINDERS.contains(&self.reminder) {
            return Err(
                "reminder is -1 for none, or 0, 5, 10, 15, 30, 60 or 1440 minutes before the start"
                    .into(),
            );
        }
        if Recurrence::from_rrule(&self.rrule).to_rrule() != self.rrule {
            return Err(
                "rrule is empty for an event that does not repeat, or one of FREQ=DAILY, \
                 FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR, FREQ=WEEKLY, FREQ=MONTHLY, FREQ=YEARLY"
                    .into(),
            );
        }
        if !self.project.is_empty() && self.project.parse::<Id>().is_err() {
            return Err("project is the id of one of the calendar's projects".into());
        }
        if self.categories.len() > crate::model::MAX_CATEGORIES {
            return Err("categories are at most 32".into());
        }
        Ok(())
    }

    /// The event this describes.
    ///
    /// # Errors
    /// Which of its fields no calendar can hold, and what that field takes.
    pub fn event(&self) -> Result<Event, String> {
        const DATE: &str = "a date such as 2026-11-20";
        let id = self.id.parse::<Id>().map_err(|_| {
            "id is the event's id as the calendar lists it; leave it out for a new event".to_owned()
        })?;
        let (start, end, timezone) = self.moments()?;
        let (attendees, organizer) = self.guests()?;
        if !crate::timezone::valid_span(&timezone, start, end, self.end_later_fold) {
            return Err("end comes at or after start".into());
        }
        self.choices()?;
        let categories = self
            .categories
            .iter()
            .map(|held| crate::model::category(held))
            .collect::<Option<_>>()
            .ok_or_else(|| "categories are each a few words of at most 64 characters".to_owned())?;
        Ok(Event {
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
            recurrence_until: parse_optional_date(&self.recurrence_until)
                .ok_or_else(|| format!("recurrence_until is {DATE}"))?,
            recurrence_count: (self.recurrence_count != 0).then_some(self.recurrence_count),
            recurrence_exdates: self
                .recurrence_exdates
                .iter()
                .map(|date| tilo_formats::date_from_text(date))
                .collect::<Option<_>>()
                .ok_or_else(|| format!("recurrence_exdates are each {DATE}"))?,
            occurrence_overrides: self
                .occurrence_overrides
                .iter()
                .map(OccurrenceOverrideData::to_override)
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    format!(
                        "occurrence_overrides each name the date they change as {DATE} and \
                         their times as a date and time such as 2026-11-20T09:00"
                    )
                })?
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
            categories,
        })
    }
}

#[allow(
    clippy::option_option,
    reason = "outer option rejects malformed input; inner option represents an explicit empty value"
)]
fn parse_optional_date(value: &str) -> Option<Option<Date>> {
    if value.is_empty() {
        Some(None)
    } else {
        tilo_formats::date_from_text(value).map(Some)
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
            id: if self.id.trim().is_empty() {
                crate::ics::stable_external_attendee_id(email)
            } else {
                self.id.parse().ok()?
            },
            name: self.name.trim().to_string(),
            email: email_address(email)?.to_string(),
            status: InviteStatus::from_ical(&self.status),
        })
    }
}

impl OccurrenceOverrideData {
    fn from_override(value: &OccurrenceOverride) -> Self {
        Self {
            original_date: value.original_date.to_string(),
            original_time: value
                .original_time
                .map_or_else(String::new, tilo_formats::clock_text),
            cancelled: value.cancelled,
            start: value
                .start
                .map_or_else(String::new, tilo_formats::minute_text),
            end: value
                .end
                .map_or_else(String::new, tilo_formats::minute_text),
            end_later_fold: value.end_later_fold,
            title: value.title.clone().unwrap_or_default(),
            location: value.location.clone().unwrap_or_default(),
        }
    }

    fn to_override(&self) -> Option<OccurrenceOverride> {
        let start = if self.start.is_empty() {
            None
        } else {
            Some(tilo_formats::minute_from_text(&self.start)?)
        };
        let end = if self.end.is_empty() {
            None
        } else {
            Some(tilo_formats::minute_from_text(&self.end)?)
        };
        if !self.end_later_fold && start.zip(end).is_some_and(|(start, end)| start > end) {
            return None;
        }
        Some(OccurrenceOverride {
            original_date: tilo_formats::date_from_text(&self.original_date)?,
            original_time: if self.original_time.is_empty() {
                None
            } else {
                Some(tilo_formats::clock_from_text(&self.original_time)?)
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TaskData {
    pub id: String,
    pub title: String,
    pub done: bool,
    pub due: String,
    pub priority: u8,
    pub project: String,
    pub parent: String,
}

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
            due: t.due.map(tilo_formats::date_text).unwrap_or_default(),
            priority: t.priority.rank(),
            project: t.project.map(|p| p.to_string()).unwrap_or_default(),
            parent: t.parent.map(|p| p.to_string()).unwrap_or_default(),
        }
    }

    pub fn to_task(&self) -> Option<Task> {
        let id = self.id.parse::<Id>().ok()?;
        if self.priority > Priority::None.rank()
            || (!self.due.is_empty() && tilo_formats::date_from_text(&self.due).is_none())
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
                .then(|| tilo_formats::date_from_text(&self.due))
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

pub fn import_payload(source: &str) -> Option<(Vec<EventData>, Vec<TaskData>)> {
    import_parsed(&crate::ics::parse_calendar(source))
}

pub(crate) fn import_parsed(
    parsed: &crate::ics::Parsed,
) -> Option<(Vec<EventData>, Vec<TaskData>)> {
    let events: Vec<_> = parsed.events.iter().map(EventData::from_event).collect();
    let tasks: Vec<_> = parsed.tasks.iter().map(TaskData::from_task).collect();
    (!events.is_empty() || !tasks.is_empty()).then_some((events, tasks))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarCollectionResource {
    pub id: String,
    pub label: String,
    pub color: Option<usize>,
    pub visible: bool,
}

/// What importing `source` into the calendar asks: the events and tasks the
/// file names, and the time zones it names that nothing here knows.
pub fn import(source: &str) -> Option<tilo_resource::Invoke<Calendar>> {
    import_reporting(source).map(|(import, _)| import)
}

/// [`import`], with the unknown time zones the file named.
pub fn import_reporting(source: &str) -> Option<(tilo_resource::Invoke<Calendar>, Vec<String>)> {
    let parsed = crate::ics::parse_calendar(source);
    let (events, tasks) = import_parsed(&parsed)?;
    Some((
        Calendar::IMPORT_ITEMS.with(crate::calendar::ImportItems { events, tasks }),
        parsed.unknown_zones,
    ))
}

/// What a window says it imported `file`, and which zones it could not read.
pub fn imported_status(file: &str, unknown_zones: &[String]) -> String {
    match unknown_zones {
        [] => format!("Imported calendar items from {file}"),
        [zone] => format!(
            "Imported calendar items from {file}. Times in {zone} stay as written, as that time zone is unknown"
        ),
        _ => format!(
            "Imported calendar items from {file}. Times in unknown time zones stay as written"
        ),
    }
}
