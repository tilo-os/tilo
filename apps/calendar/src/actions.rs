//! The calendar's exported **actions** and the **resource tree** it publishes to
//! Ctrl+K.
//!
//! Every document-mutating operation the calendar performs on its shared
//! events/tasks store is a [`CalendarAction`]: a small serializable value that
//! deterministically mutates the CRDT (see [`crate::doc`], where events and
//! tasks live as keyed CRDT maps). Because the store already lives in the shared
//! document, an action replicates and replays for free. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the New button, the
//! editor's field edits, the trash button, a task's checkbox) dispatch and what
//! the Ctrl+K palette dispatches — one source of truth, one replay path.
//!
//! The resource tree mirrors the data model: the calendar owns its events and
//! its tasks, and each exports the [`ActionSpec`]s Ctrl+K offers.
//!
//! Presentation — which view is showing, the selected day, an open popover, the
//! scroll position — stays out of the document and never becomes an action.
//!
//! ## Determinism contract
//!
//! Ids are minted at *authoring* time and carried in the payload; timestamps and
//! recurrence/colour values are resolved to their document representation before
//! the action is built. [`Action::apply`] never touches the RNG or the
//! wall-clock, and a malformed payload (an unparseable id or datetime) is a
//! **no-op** rather than an error, so a bad replay can't wedge a peer.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};
use tilo_document::RichTextAction;
use ui::tilo_resource::Effect as _;
use uuid::Uuid;

use tilo_actions::{
    Action, ActionAddress, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus,
};

use crate::doc::CalDoc;
use crate::model::{
    Attendee, Event, Id, InviteStatus, OccurrenceOverride, Palette, Priority, Project, Recurrence,
    Task,
};

/// Registry `type_id` for every calendar action.
pub const CALENDAR_ACTION_TYPE: &str = "calendar.action";

/// A task delete action carries every child id it will tombstone, so cap that
/// payload independently from directory paging. Larger child sets remain
/// addressable one-by-one but require their children to be handled first.
pub(crate) const TASK_DELETE_FANOUT_LIMIT: usize = 96;

/// Stable resource id for one event. Search, the palette, accessibility and
/// agents all address this same resource.
pub fn event_resource_id(id: Id) -> String {
    format!("calendar.event:{id}")
}

/// Stable resource id for one task.
pub fn task_resource_id(id: Id) -> String {
    format!("calendar.task:{id}")
}

/// Stable resource id for one persisted task project.
pub fn project_resource_id(id: Id) -> String {
    format!("calendar.project:{id}")
}

/// Stable resource id for a local or configured remote calendar collection.
pub fn calendar_resource_id(id: &str) -> String {
    format!("calendar.collection:{id}")
}

pub(crate) fn project_id_from_resource(address: &str) -> Option<Id> {
    address.strip_prefix("calendar.project:")?.parse().ok()
}

pub(crate) fn calendar_id_from_resource(address: &str) -> Option<&str> {
    address
        .strip_prefix("calendar.collection:")
        .filter(|id| !id.is_empty())
}

pub fn attendee_resource_id(event_id: Id, attendee_id: Id) -> String {
    format!("calendar.attendee:{event_id}:{attendee_id}")
}

pub fn occurrence_resource_id(event_id: Id, original_date: NaiveDate) -> String {
    format!("calendar.occurrence:{event_id}:{original_date}")
}

pub fn reminder_resource_id(event_id: Id) -> String {
    format!("calendar.reminder:{event_id}:primary")
}

/// Canonical view action that reveals one event in the calendar.
pub fn open_event_address(id: Id) -> ActionAddress {
    ActionAddress::new(event_resource_id(id), "open")
}

/// Canonical view action that reveals one task in the calendar.
pub fn open_task_address(id: Id) -> ActionAddress {
    ActionAddress::new(task_resource_id(id), "open")
}

/// Canonical view action that opens a task project in the Calendar task list.
pub fn open_project_address(id: Id) -> ActionAddress {
    ActionAddress::new(project_resource_id(id), "open")
}

/// Opening an item — an event, an occurrence, a task, a project or a
/// calendar — as the item offers it.
pub(crate) fn open_spec(item: String) -> ActionSpec {
    crate::resource::CalendarEffect::Open { item }.bound()
}

/// A calendar collection as a live row: open it, show or hide it.
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
}

/// The datetime format events are stored in — matches [`crate::doc`].
pub(crate) const DT_FMT: &str = "%Y-%m-%dT%H:%M";
/// The date format tasks store their due date in.
pub(crate) const DATE_FMT: &str = "%Y-%m-%d";

/// A serializable snapshot of an event's scalar fields, in exactly the
/// representation the document stores (datetimes as `DT_FMT` strings, colour as
/// a palette index, recurrence as an RRULE body, an absent reminder as `-1`).
/// Carrying the document shape keeps [`Action::apply`] a pure write with no
/// re-derivation, and makes [`Self::from_event`]/`Self::to_event` lossless
/// round-trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventData {
    pub id: String,
    pub title: String,
    pub start: String,
    pub end: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendeeData {
    pub id: String,
    pub name: String,
    pub email: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrenceOverrideData {
    pub original_date: String,
    #[serde(default)]
    pub original_time: String,
    pub cancelled: bool,
    pub start: String,
    pub end: String,
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
    fn to_event(&self) -> Option<Event> {
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
        if start > end
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
                .collect::<Result<Vec<_>, _>>()
                .ok()?,
            occurrence_overrides: self
                .occurrence_overrides
                .iter()
                .map(OccurrenceOverrideData::to_override)
                .collect::<Option<Vec<_>>>()?,
            timezone,
            organizer,
            attendees,
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
                .map_or_else(String::new, |time| time.format("%H:%M:%S").to_string()),
            cancelled: value.cancelled,
            start: value
                .start
                .map_or_else(String::new, |date| date.format(DT_FMT).to_string()),
            end: value
                .end
                .map_or_else(String::new, |date| date.format(DT_FMT).to_string()),
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
        if start.zip(end).is_some_and(|(start, end)| start > end) {
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
            title: (!self.title.is_empty()).then(|| self.title.clone()),
            location: (!self.location.is_empty()).then(|| self.location.clone()),
        })
    }
}

/// A serializable snapshot of a task's scalar fields, in the document's shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Serializable scalar representation of a persisted Calendar task project.
/// Projects are real collaborative entities, not just labels attached to
/// tasks, so agents and search address them with their own stable resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectData {
    pub id: String,
    pub name: String,
    pub color: usize,
    pub order: u32,
}

impl ProjectData {
    pub fn from_project(project: &Project) -> Self {
        Self {
            id: project.id.to_string(),
            name: project.name.clone(),
            color: project.color.index(),
            order: project.order,
        }
    }

    fn to_project(&self) -> Option<Project> {
        let id = self.id.parse::<Id>().ok()?;
        (!self.name.trim().is_empty() && self.color < Palette::ALL.len()).then_some(Project {
            id,
            name: self.name.clone(),
            color: Palette::from_index(self.color),
            order: self.order,
        })
    }
}

/// A sparse, field-level change to one event. `None` means “leave this CRDT
/// register untouched”; an empty calendar id explicitly means the local
/// calendar. This is the collaboration-safe counterpart to `EventData`:
/// independent title, location, reminder, or time edits commute instead of a
/// stale whole-event snapshot rewriting neighbouring fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPatch {
    pub title: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub all_day: Option<bool>,
    pub location: Option<String>,
    pub color: Option<usize>,
    pub rrule: Option<String>,
    pub recurrence_interval: Option<u32>,
    /// `""` clears the inclusive recurrence end date.
    pub recurrence_until: Option<String>,
    /// `0` clears the occurrence limit.
    pub recurrence_count: Option<u32>,
    pub timezone: Option<String>,
    /// `""` clears the organizer.
    pub organizer: Option<String>,
    pub reminder: Option<i64>,
    /// Project id, or an empty string to remove the project association.
    pub project: Option<String>,
    pub calendar_id: Option<String>,
    /// Only calendar moves affect the remote outbox. `Some(true)` queues a
    /// remote upload; `Some(false)` removes a now-local event from it.
    pub enqueue_remote: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendeePatch {
    pub name: Option<String>,
    pub email: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrencePatch {
    pub cancelled: Option<bool>,
    /// Empty strings clear an override and inherit the series value.
    pub start: Option<String>,
    pub end: Option<String>,
    pub title: Option<String>,
    pub location: Option<String>,
}

/// A sparse, field-level change to one task. Keeping task edits as patches is
/// as important as event edits: a collaborator changing a due date must not
/// accidentally overwrite an independently changed title or priority.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPatch {
    pub title: Option<String>,
    /// Due date in `YYYY-MM-DD` form, or an empty string to clear it.
    pub due: Option<String>,
    /// Stored [`Priority::rank`] value.
    pub priority: Option<u8>,
    /// Project id, or an empty string to remove the project association.
    pub project: Option<String>,
}

/// Sparse project edit. Distinct registers mean one collaborator can rename a
/// project while another adjusts its colour/order without a stale snapshot
/// rewriting either change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectPatch {
    pub name: Option<String>,
    pub color: Option<usize>,
    pub order: Option<u32>,
}

impl EventPatch {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Derive the exact registers changed by an on-screen mutation. Keeping
    /// this comparison next to the serializable action makes every caller use
    /// the same merge-safe semantics.
    #[must_use]
    pub fn between(before: &Event, after: &Event) -> Self {
        Self {
            title: (before.title != after.title).then(|| after.title.clone()),
            start: (before.start != after.start).then(|| after.start.format(DT_FMT).to_string()),
            end: (before.end != after.end).then(|| after.end.format(DT_FMT).to_string()),
            all_day: (before.all_day != after.all_day).then_some(after.all_day),
            location: (before.location != after.location).then(|| after.location.clone()),
            color: (before.color != after.color).then_some(after.color.index()),
            rrule: (before.recurrence != after.recurrence).then(|| after.recurrence.to_rrule()),
            recurrence_interval: (before.recurrence_interval != after.recurrence_interval)
                .then_some(after.recurrence_interval),
            recurrence_until: (before.recurrence_until != after.recurrence_until).then(|| {
                after
                    .recurrence_until
                    .map_or_else(String::new, |date| date.to_string())
            }),
            recurrence_count: (before.recurrence_count != after.recurrence_count)
                .then_some(after.recurrence_count.unwrap_or(0)),
            timezone: (before.timezone != after.timezone).then(|| after.timezone.clone()),
            organizer: (before.organizer != after.organizer).then(|| {
                after
                    .organizer
                    .map_or_else(String::new, |id| id.to_string())
            }),
            reminder: (before.reminder_min != after.reminder_min)
                .then_some(after.reminder_min.unwrap_or(-1)),
            project: (before.project != after.project).then(|| {
                after
                    .project
                    .map(|project| project.to_string())
                    .unwrap_or_default()
            }),
            calendar_id: (before.calendar_id != after.calendar_id)
                .then(|| after.calendar_id.clone().unwrap_or_default()),
            enqueue_remote: None,
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

    fn to_task(&self) -> Option<Task> {
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

/// Every collaborative operation the calendar exports on its events/tasks store.
///
/// Each variant is a whole-item write or delete carrying enough payload to
/// replay deterministically from a shared base state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CalendarAction {
    /// Create or replace a Calendar task project. Only creation/import-like
    /// paths use this full snapshot; ordinary edits use [`ProjectPatch`].
    UpsertProject(ProjectData),
    /// Mutate only the project registers explicitly carried in the action.
    PatchProject { id: String, patch: ProjectPatch },
    /// Remove an unused project. Referenced projects are refused rather than
    /// silently detaching events/tasks through a surprising bulk mutation.
    DeleteProject { id: String },
    /// Create or replace an event's scalar fields. One variant covers create,
    /// every field edit, and a move (a start/end change is just new datetimes).
    UpsertEvent(EventData),
    /// Update only named event registers. This is what all ordinary editor and
    /// agent field edits use; `UpsertEvent` is reserved for creation/import.
    PatchEvent { id: String, patch: EventPatch },
    /// Add one stable invitation participant. Ordinary participant edits use
    /// `PatchAttendee` so independent RSVP/name changes do not overwrite each
    /// other or the event.
    UpsertAttendee {
        event_id: String,
        attendee: AttendeeData,
    },
    PatchAttendee {
        event_id: String,
        attendee_id: String,
        patch: AttendeePatch,
    },
    DeleteAttendee {
        event_id: String,
        attendee_id: String,
    },
    /// Include/exclude one original recurrence date by absolute identity.
    SetRecurrenceException {
        event_id: String,
        date: String,
        excluded: bool,
    },
    /// Change one generated occurrence, keyed by its immutable original date.
    PatchOccurrence {
        event_id: String,
        original_date: String,
        patch: OccurrencePatch,
    },
    /// Delete an event, clearing any queued upload. When the event was synced
    /// from a still-configured remote calendar, `remote_delete` carries that
    /// calendar id so `apply` also queues a `CalDAV` delete of the server copy.
    DeleteEvent {
        id: String,
        remote_delete: Option<String>,
    },
    /// Create or replace a task's scalar fields.
    UpsertTask(TaskData),
    /// Update only named task registers. This avoids a stale task snapshot
    /// clobbering fields changed on another device.
    PatchTask { id: String, patch: TaskPatch },
    /// Set a task's completion to an absolute value resolved at authoring
    /// time. Relative toggles are unsafe across independently advancing
    /// replicas: this action converges no matter which history it reaches.
    SetTaskDone { id: String, done: bool },
    /// Delete a task and each of its listed subtasks (resolved at authoring
    /// time from the model, carried here so `apply` needs no lookup).
    DeleteTask { id: String, subtasks: Vec<String> },
    /// One rich-text edit of an event's or a task's notes. This is the only
    /// Calendar path allowed to change notes.
    EditNotes {
        owner: NoteOwner,
        edit: RichTextAction,
    },
    /// A single deterministic import batch. The external file is parsed before
    /// this action is authored; replay only writes the exact payload captured
    /// here, and has one history/undo boundary.
    ImportIcs {
        events: Vec<EventData>,
        tasks: Vec<TaskData>,
    },
    /// Persist the server href and etag for one local event or task.
    SetRemoteBinding {
        id: String,
        calendar_id: String,
        href: String,
        etag: String,
    },
    /// Write an event learned from the server. Unlike [`Self::UpsertEvent`],
    /// this does not queue the event for upload.
    AdoptRemoteEvent(EventData),
    /// Write a task learned from the server.
    AdoptRemoteTask(TaskData),
    /// Remove an item the server no longer has. Does not queue a delete.
    DropRemoteItem { id: String },
    /// A successful upload left the push queue.
    AcknowledgeRemotePush { id: String },
    /// The server confirmed a delete, so the local delete queue and binding go.
    AcknowledgeRemoteDelete { id: String },
    /// Remember the collection sync token after a reconciled pull.
    SetSyncToken { calendar_id: String, token: String },
}

/// The concrete owner of a Calendar note document. Keeping ownership in the
/// payload makes stale/deleted-target refusal deterministic during replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteOwner {
    Event { id: String },
    Task { id: String },
}

/// Durable, browseable history metadata for a calendar mutation.
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive action-to-history metadata table is clearer kept beside the action enum"
)]
pub fn history_context(action: &CalendarAction) -> tilo_actions::ActionContext {
    let mut context =
        tilo_actions::ActionContext::for_action("so.tilo.calendar", "Calendar", action);
    let (label, id, kind, title) = match action {
        CalendarAction::UpsertProject(project) => (
            "Update project",
            project.id.clone(),
            "calendar.project",
            project.name.clone(),
        ),
        CalendarAction::PatchProject { id, .. } => {
            ("Update project", id.clone(), "calendar.project", id.clone())
        }
        CalendarAction::DeleteProject { id } => {
            ("Delete project", id.clone(), "calendar.project", id.clone())
        }
        CalendarAction::UpsertEvent(event) => (
            "Update event",
            event.id.clone(),
            "calendar.event",
            event.title.clone(),
        ),
        CalendarAction::PatchEvent { id, patch } => {
            let label = if patch.calendar_id.is_some() {
                "Move event"
            } else {
                "Update event"
            };
            (label, id.clone(), "calendar.event", id.clone())
        }
        CalendarAction::UpsertAttendee {
            event_id: _,
            attendee,
        } => (
            "Invite attendee",
            attendee.id.clone(),
            "calendar.attendee",
            attendee.email.clone(),
        ),
        CalendarAction::PatchAttendee { attendee_id, .. } => (
            "Update attendee",
            attendee_id.clone(),
            "calendar.attendee",
            attendee_id.clone(),
        ),
        CalendarAction::DeleteAttendee { attendee_id, .. } => (
            "Remove attendee",
            attendee_id.clone(),
            "calendar.attendee",
            attendee_id.clone(),
        ),
        CalendarAction::SetRecurrenceException { event_id, date, .. }
        | CalendarAction::PatchOccurrence {
            event_id,
            original_date: date,
            ..
        } => (
            "Update occurrence",
            format!("{event_id}:{date}"),
            "calendar.occurrence",
            date.clone(),
        ),
        CalendarAction::DeleteEvent { id, .. } => {
            ("Delete event", id.clone(), "calendar.event", id.clone())
        }
        CalendarAction::UpsertTask(task) => (
            "Update task",
            task.id.clone(),
            "calendar.task",
            task.title.clone(),
        ),
        CalendarAction::PatchTask { id, .. } => {
            ("Update task", id.clone(), "calendar.task", id.clone())
        }
        CalendarAction::SetTaskDone { id, done } => (
            if *done {
                "Complete task"
            } else {
                "Reopen task"
            },
            id.clone(),
            "calendar.task",
            id.clone(),
        ),
        CalendarAction::DeleteTask { id, .. } => {
            ("Delete task", id.clone(), "calendar.task", id.clone())
        }
        CalendarAction::EditNotes { owner, .. } => match owner {
            NoteOwner::Event { id } => {
                ("Edit event notes", id.clone(), "calendar.event", id.clone())
            }
            NoteOwner::Task { id } => ("Edit task notes", id.clone(), "calendar.task", id.clone()),
        },
        CalendarAction::ImportIcs { .. } => (
            "Import calendar",
            "calendar".into(),
            "calendar.calendar",
            "Calendar".into(),
        ),
        CalendarAction::SetRemoteBinding { id, href, .. } => (
            "Record remote binding",
            id.clone(),
            "calendar.event",
            href.clone(),
        ),
        CalendarAction::AdoptRemoteEvent(event) => (
            "Adopt remote event",
            event.id.clone(),
            "calendar.event",
            event.title.clone(),
        ),
        CalendarAction::AdoptRemoteTask(task) => (
            "Adopt remote task",
            task.id.clone(),
            "calendar.task",
            task.title.clone(),
        ),
        CalendarAction::DropRemoteItem { id } => {
            ("Drop remote item", id.clone(), "calendar.event", id.clone())
        }
        CalendarAction::AcknowledgeRemotePush { id } => (
            "Acknowledge remote upload",
            id.clone(),
            "calendar.event",
            id.clone(),
        ),
        CalendarAction::AcknowledgeRemoteDelete { id } => (
            "Acknowledge remote delete",
            id.clone(),
            "calendar.event",
            id.clone(),
        ),
        CalendarAction::SetSyncToken { calendar_id, .. } => (
            "Record sync token",
            calendar_id.clone(),
            "calendar.calendar",
            calendar_id.clone(),
        ),
    };
    context.action_label = label.to_string();
    context.resource = Some(tilo_actions::ActionResource {
        id: if kind == "calendar.calendar" {
            kind.to_string()
        } else {
            format!("{kind}:{id}")
        },
        kind: kind.to_string(),
        label: title,
    });
    context
}

/// Name an existing resource from the committed document before an edit or
/// deletion, so platform History identifies the event, task or project a
/// person changed rather than showing its opaque storage key.
pub fn history_context_in_document(
    action: &CalendarAction,
    doc: &ui::automerge::AutoCommit,
) -> tilo_actions::ActionContext {
    let mut context = history_context(action);
    let Some(resource) = &mut context.resource else {
        return context;
    };
    let Some((_, key)) = resource.id.split_once(':') else {
        return context;
    };
    // Upserts already carry their new human-readable title in the action.
    if resource.label != key && resource.kind != "calendar.occurrence" {
        return context;
    }
    let Ok(id) = key.split(':').next().unwrap_or_default().parse::<Id>() else {
        return context;
    };
    let model = CalDoc::read_model(doc);
    let title = match resource.kind.as_str() {
        "calendar.project" => model.project(id).map(|project| project.name.clone()),
        "calendar.event" => model.event(id).map(|event| event.title.clone()),
        "calendar.task" => model.task(id).map(|task| task.title.clone()),
        "calendar.attendee" => model
            .events
            .iter()
            .flat_map(|event| &event.attendees)
            .find(|attendee| attendee.id == id)
            .map(|attendee| attendee.email.clone()),
        "calendar.occurrence" => model
            .event(id)
            .map(|event| format!("{} — {}", event.title, resource.label)),
        _ => None,
    };
    if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
        resource.label = title;
    }
    context
}

fn refused(message: &str) -> ActionError {
    ActionError::Refused(message.to_string())
}

fn validate_project_patch(doc: &Doc, id: Id, patch: &ProjectPatch) -> Result<(), ActionError> {
    if CalDoc::read_model(doc).project(id).is_none() {
        return Err(refused("project no longer exists"));
    }
    if patch
        .name
        .as_ref()
        .is_some_and(|name| name.trim().is_empty())
    {
        return Err(refused("project name must contain non-whitespace text"));
    }
    if patch.color.is_some_and(|color| color >= Palette::ALL.len()) {
        return Err(refused("project color is invalid"));
    }
    Ok(())
}

fn apply_upsert_project(doc: &mut Doc, data: &ProjectData) {
    if let Some(project) = data.to_project() {
        CalDoc::update_project(doc, &project);
    }
}

fn apply_patch_project(doc: &mut Doc, id: &str, patch: &ProjectPatch) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("project id is invalid"))?;
    validate_project_patch(doc, id, patch)?;
    CalDoc::patch_project(doc, id, patch);
    Ok(())
}

fn apply_delete_project(doc: &mut Doc, id: &str) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("project id is invalid"))?;
    let model = CalDoc::read_model(doc);
    if model.project(id).is_none() {
        return Err(refused("project no longer exists"));
    }
    if model.events.iter().any(|event| event.project == Some(id))
        || model.tasks.iter().any(|task| task.project == Some(id))
    {
        return Err(refused(
            "move or unfile linked events and tasks before deleting this project",
        ));
    }
    CalDoc::delete_project(doc, id);
    Ok(())
}

fn validate_event_patch(doc: &Doc, id: Id, patch: &EventPatch) -> Result<(), ActionError> {
    let model = CalDoc::read_model(doc);
    let Some(event) = model.event(id) else {
        return Err(refused("event no longer exists"));
    };
    let start = patch
        .start
        .as_deref()
        .map(|value| NaiveDateTime::parse_from_str(value, DT_FMT))
        .transpose()
        .map_err(|_| refused("event start is invalid"))?
        .unwrap_or(event.start);
    let end = patch
        .end
        .as_deref()
        .map(|value| NaiveDateTime::parse_from_str(value, DT_FMT))
        .transpose()
        .map_err(|_| refused("event end is invalid"))?
        .unwrap_or(event.end);
    if start > end {
        return Err(refused("event end must not precede start"));
    }
    if patch.color.is_some_and(|color| color >= Palette::ALL.len()) {
        return Err(refused("event color is invalid"));
    }
    if patch
        .reminder
        .is_some_and(|minutes| !matches!(minutes, -1 | 10 | 30 | 60))
    {
        return Err(refused("event reminder is unsupported"));
    }
    if patch
        .rrule
        .as_ref()
        .is_some_and(|rule| Recurrence::from_rrule(rule).to_rrule() != *rule)
    {
        return Err(refused("event recurrence is unsupported"));
    }
    if patch.recurrence_interval == Some(0) {
        return Err(refused("recurrence interval must be positive"));
    }
    if let Some(until) = &patch.recurrence_until
        && !until.is_empty()
        && NaiveDate::parse_from_str(until, DATE_FMT).is_err()
    {
        return Err(refused("recurrence end date is invalid"));
    }
    if let Some(timezone) = &patch.timezone
        && normalize_timezone(timezone).is_none()
    {
        return Err(refused("event timezone is invalid"));
    }
    if let Some(organizer) = &patch.organizer
        && !organizer.is_empty()
        && organizer
            .parse::<Id>()
            .ok()
            .is_none_or(|id| !event.attendees.iter().any(|attendee| attendee.id == id))
    {
        return Err(refused("organizer must be an event attendee"));
    }
    if let Some(project) = &patch.project {
        let project = (!project.is_empty())
            .then(|| project.parse::<Id>().ok())
            .flatten();
        if (!patch.project.as_deref().unwrap_or_default().is_empty() && project.is_none())
            || !CalDoc::project_exists(doc, project)
        {
            return Err(refused("event project is unavailable"));
        }
    }
    if patch.enqueue_remote == Some(true)
        && patch
            .calendar_id
            .as_deref()
            .unwrap_or_else(|| event.calendar_id.as_deref().unwrap_or_default())
            .is_empty()
    {
        return Err(refused("a local event cannot be queued for remote sync"));
    }
    Ok(())
}

fn event_and_attendee(
    doc: &Doc,
    event_id: &str,
    attendee_id: &str,
) -> Result<(Id, Id, Event), ActionError> {
    let event_id = event_id
        .parse::<Id>()
        .map_err(|_| refused("event id is invalid"))?;
    let attendee_id = attendee_id
        .parse::<Id>()
        .map_err(|_| refused("attendee id is invalid"))?;
    let event = CalDoc::read_model(doc)
        .event(event_id)
        .cloned()
        .ok_or_else(|| refused("event no longer exists"))?;
    Ok((event_id, attendee_id, event))
}

fn apply_upsert_attendee(
    doc: &mut Doc,
    event_id: &str,
    data: &AttendeeData,
) -> Result<(), ActionError> {
    let event_id = event_id
        .parse::<Id>()
        .map_err(|_| refused("event id is invalid"))?;
    if CalDoc::read_model(doc).event(event_id).is_none() {
        return Err(refused("event no longer exists"));
    }
    let attendee = data
        .to_attendee()
        .ok_or_else(|| refused("attendee is invalid"))?;
    CalDoc::upsert_attendee(doc, event_id, &attendee);
    Ok(())
}

fn apply_patch_attendee(
    doc: &mut Doc,
    event_id: &str,
    attendee_id: &str,
    patch: &AttendeePatch,
) -> Result<(), ActionError> {
    let (event_id, attendee_id, event) = event_and_attendee(doc, event_id, attendee_id)?;
    if !event
        .attendees
        .iter()
        .any(|attendee| attendee.id == attendee_id)
    {
        return Err(refused("attendee no longer exists"));
    }
    if patch
        .email
        .as_deref()
        .is_some_and(|email| email_address(email.trim()).is_none())
    {
        return Err(refused("attendee email is invalid"));
    }
    if patch.status.as_deref().is_some_and(|status| {
        !matches!(
            status,
            "NEEDS-ACTION" | "ACCEPTED" | "TENTATIVE" | "DECLINED"
        )
    }) {
        return Err(refused("attendee response is invalid"));
    }
    CalDoc::patch_attendee(doc, event_id, attendee_id, patch);
    Ok(())
}

fn apply_delete_attendee(
    doc: &mut Doc,
    event_id: &str,
    attendee_id: &str,
) -> Result<(), ActionError> {
    let (event_id, attendee_id, event) = event_and_attendee(doc, event_id, attendee_id)?;
    if !event
        .attendees
        .iter()
        .any(|attendee| attendee.id == attendee_id)
    {
        return Err(refused("attendee no longer exists"));
    }
    CalDoc::delete_attendee(doc, event_id, attendee_id);
    Ok(())
}

fn apply_recurrence_exception(
    doc: &mut Doc,
    event_id: &str,
    date: &str,
    excluded: bool,
) -> Result<(), ActionError> {
    let event_id = event_id
        .parse::<Id>()
        .map_err(|_| refused("event id is invalid"))?;
    let date = NaiveDate::parse_from_str(date, DATE_FMT)
        .map_err(|_| refused("occurrence date is invalid"))?;
    if CalDoc::read_model(doc).event(event_id).is_none() {
        return Err(refused("event no longer exists"));
    }
    CalDoc::set_exception(doc, event_id, date, excluded);
    Ok(())
}

fn apply_occurrence_patch(
    doc: &mut Doc,
    event_id: &str,
    original_date: &str,
    patch: &OccurrencePatch,
) -> Result<(), ActionError> {
    let event_id = event_id
        .parse::<Id>()
        .map_err(|_| refused("event id is invalid"))?;
    let original_date = NaiveDate::parse_from_str(original_date, DATE_FMT)
        .map_err(|_| refused("occurrence date is invalid"))?;
    if CalDoc::read_model(doc).event(event_id).is_none() {
        return Err(refused("event no longer exists"));
    }
    let start = patch
        .start
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| NaiveDateTime::parse_from_str(value, DT_FMT))
        .transpose()
        .map_err(|_| refused("occurrence start is invalid"))?;
    let end = patch
        .end
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| NaiveDateTime::parse_from_str(value, DT_FMT))
        .transpose()
        .map_err(|_| refused("occurrence end is invalid"))?;
    if start.zip(end).is_some_and(|(start, end)| start > end) {
        return Err(refused("occurrence end must not precede start"));
    }
    CalDoc::patch_occurrence_override(doc, event_id, original_date, patch);
    Ok(())
}

fn validate_task_patch(doc: &Doc, id: Id, patch: &TaskPatch) -> Result<(), ActionError> {
    if CalDoc::read_model(doc).task(id).is_none() {
        return Err(refused("task no longer exists"));
    }
    if patch
        .due
        .as_ref()
        .is_some_and(|due| !due.is_empty() && NaiveDate::parse_from_str(due, DATE_FMT).is_err())
    {
        return Err(refused("task due date is invalid"));
    }
    if patch
        .priority
        .is_some_and(|priority| priority > Priority::None.rank())
    {
        return Err(refused("task priority is invalid"));
    }
    if let Some(project) = &patch.project {
        let project = (!project.is_empty())
            .then(|| project.parse::<Id>().ok())
            .flatten();
        if (!patch.project.as_deref().unwrap_or_default().is_empty() && project.is_none())
            || !CalDoc::project_exists(doc, project)
        {
            return Err(refused("task project is unavailable"));
        }
    }
    Ok(())
}

fn apply_upsert_event(doc: &mut Doc, data: &EventData) {
    if let Some(event) = data.to_event()
        && CalDoc::project_exists(doc, event.project)
    {
        let remote = event.calendar_id.clone();
        // Undo of delete restores through UpsertEvent: clear the tombstone
        // the delete wrote so the row can be rewritten.
        CalDoc::clear_tombstone(doc, "event", event.id);
        CalDoc::update_event(doc, &event);
        // A remote-calendar event is an outside consequence: the push outbox
        // is the projection the live window drains to its server. Undo of a
        // remote delete restores through UpsertEvent and must re-queue the
        // upload the same way.
        if remote.is_some() {
            CalDoc::enqueue_push(doc, event.id);
        }
    }
}

fn apply_patch_event(doc: &mut Doc, id: &str, patch: &EventPatch) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("event id is invalid"))?;
    validate_event_patch(doc, id, patch)?;
    CalDoc::patch_event(doc, id, patch);
    if let Some(enqueue) = patch.enqueue_remote {
        if enqueue {
            CalDoc::enqueue_push(doc, id);
        } else {
            CalDoc::dequeue_push(doc, id);
        }
    }
    Ok(())
}

fn apply_delete_event(
    doc: &mut Doc,
    id: &str,
    remote_delete: Option<&String>,
) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("event id is invalid"))?;
    let model = CalDoc::read_model(doc);
    let event = model
        .event(id)
        .ok_or_else(|| refused("event no longer exists"))?;
    if remote_delete.map(String::as_str) != event.calendar_id.as_deref() {
        return Err(refused(
            "remote delete does not match the event source calendar",
        ));
    }
    CalDoc::delete_event(doc, id);
    CalDoc::dequeue_push(doc, id);
    if let Some(calendar) = remote_delete {
        CalDoc::enqueue_delete(doc, calendar, id);
    }
    Ok(())
}

fn apply_upsert_task(doc: &mut Doc, data: &TaskData) {
    if let Some(task) = data.to_task()
        && CalDoc::project_exists(doc, task.project)
    {
        CalDoc::update_task(doc, &task);
    }
}

fn apply_patch_task(doc: &mut Doc, id: &str, patch: &TaskPatch) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("task id is invalid"))?;
    validate_task_patch(doc, id, patch)?;
    CalDoc::patch_task(doc, id, patch);
    Ok(())
}

fn apply_set_task_done(doc: &mut Doc, id: &str, done: bool) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("task id is invalid"))?;
    if CalDoc::read_model(doc).task(id).is_none() {
        return Err(refused("task no longer exists"));
    }
    CalDoc::set_task_done(doc, id, done);
    Ok(())
}

fn apply_delete_task(doc: &mut Doc, id: &str, subtasks: &[String]) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("task id is invalid"))?;
    let model = CalDoc::read_model(doc);
    if model.task(id).is_none() {
        return Err(refused("task no longer exists"));
    }
    let children = model.subtasks_of(id);
    let child_ids = subtasks
        .iter()
        .map(|subtask| {
            subtask
                .parse::<Id>()
                .map_err(|_| refused("subtask id is invalid"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if child_ids
        .iter()
        .any(|child| !children.iter().any(|task| task.id == *child))
    {
        return Err(refused(
            "delete payload names a task outside the target subtree",
        ));
    }
    CalDoc::delete_task(doc, id);
    for child in child_ids {
        CalDoc::delete_task(doc, child);
    }
    Ok(())
}

fn apply_import_ics(
    doc: &mut Doc,
    events: &[EventData],
    tasks: &[TaskData],
) -> Result<(), ActionError> {
    // Decode and validate the whole batch before the first write. A replayed
    // payload or a project removed since staging must not produce a partial
    // import that nevertheless reports success.
    let events = events
        .iter()
        .map(|data| {
            let event = data
                .to_event()
                .ok_or_else(|| refused("imported event is invalid"))?;
            if !CalDoc::project_exists(doc, event.project) {
                return Err(refused("imported event project is unavailable"));
            }
            Ok(event)
        })
        .collect::<Result<Vec<_>, ActionError>>()?;
    let tasks = tasks
        .iter()
        .map(|data| {
            let task = data
                .to_task()
                .ok_or_else(|| refused("imported task is invalid"))?;
            if !CalDoc::project_exists(doc, task.project) {
                return Err(refused("imported task project is unavailable"));
            }
            Ok(task)
        })
        .collect::<Result<Vec<_>, ActionError>>()?;
    // What the calendar already holds is not imported twice, so importing a
    // file again adds nothing. An event is the same title at the same start.
    let current = CalDoc::read_model(doc);
    for event in events {
        if current
            .events
            .iter()
            .any(|held| held.title == event.title && held.start == event.start)
        {
            continue;
        }
        CalDoc::update_event(doc, &event);
        CalDoc::seed_notes_if_empty(doc, true, event.id, &["Imported from .ics"]);
    }
    // A task under its own id replaces itself; one whose file names no
    // identity of ours is not added beside an equal one (same title and due
    // date) already held.
    for task in tasks {
        if current
            .tasks
            .iter()
            .any(|held| held.id != task.id && held.title == task.title && held.due == task.due)
        {
            continue;
        }
        CalDoc::update_task(doc, &task);
    }
    Ok(())
}

fn apply_edit_notes(
    doc: &mut Doc,
    owner: &NoteOwner,
    edit: &RichTextAction,
) -> Result<(), ActionError> {
    let (event, id) = match owner {
        NoteOwner::Event { id } => (true, id),
        NoteOwner::Task { id } => (false, id),
    };
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("note owner id is invalid"))?;
    let notes = CalDoc::notes(doc, event, id).ok_or_else(|| refused("notes no longer exist"))?;
    edit.apply_under(doc, &notes)
}

impl Action for CalendarAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            CalendarAction::UpsertProject(data) => apply_upsert_project(doc, data),
            CalendarAction::PatchProject { id, patch } => apply_patch_project(doc, id, patch)?,
            CalendarAction::DeleteProject { id } => apply_delete_project(doc, id)?,
            CalendarAction::UpsertEvent(data) => apply_upsert_event(doc, data),
            CalendarAction::PatchEvent { id, patch } => apply_patch_event(doc, id, patch)?,
            CalendarAction::UpsertAttendee { event_id, attendee } => {
                apply_upsert_attendee(doc, event_id, attendee)?;
            }
            CalendarAction::PatchAttendee {
                event_id,
                attendee_id,
                patch,
            } => apply_patch_attendee(doc, event_id, attendee_id, patch)?,
            CalendarAction::DeleteAttendee {
                event_id,
                attendee_id,
            } => apply_delete_attendee(doc, event_id, attendee_id)?,
            CalendarAction::SetRecurrenceException {
                event_id,
                date,
                excluded,
            } => apply_recurrence_exception(doc, event_id, date, *excluded)?,
            CalendarAction::PatchOccurrence {
                event_id,
                original_date,
                patch,
            } => apply_occurrence_patch(doc, event_id, original_date, patch)?,
            CalendarAction::DeleteEvent { id, remote_delete } => {
                apply_delete_event(doc, id, remote_delete.as_ref())?;
            }
            CalendarAction::UpsertTask(data) => apply_upsert_task(doc, data),
            CalendarAction::PatchTask { id, patch } => apply_patch_task(doc, id, patch)?,
            CalendarAction::SetTaskDone { id, done } => apply_set_task_done(doc, id, *done)?,
            CalendarAction::DeleteTask { id, subtasks } => apply_delete_task(doc, id, subtasks)?,
            CalendarAction::EditNotes { owner, edit } => apply_edit_notes(doc, owner, edit)?,
            CalendarAction::ImportIcs { events, tasks } => apply_import_ics(doc, events, tasks)?,
            CalendarAction::SetRemoteBinding {
                id,
                calendar_id,
                href,
                etag,
            } => apply_set_remote_binding(doc, id, calendar_id, href, etag)?,
            CalendarAction::AdoptRemoteEvent(data) => apply_adopt_remote_event(doc, data)?,
            CalendarAction::AdoptRemoteTask(data) => apply_adopt_remote_task(doc, data)?,
            CalendarAction::DropRemoteItem { id } => apply_drop_remote_item(doc, id)?,
            CalendarAction::AcknowledgeRemotePush { id } => apply_acknowledge_remote_push(doc, id)?,
            CalendarAction::AcknowledgeRemoteDelete { id } => {
                apply_acknowledge_remote_delete(doc, id)?;
            }
            CalendarAction::SetSyncToken { calendar_id, token } => {
                apply_set_sync_token(doc, calendar_id, token)?;
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }

    /// What a server sent and the bookkeeping of a sync are content from
    /// outside: they sync to everyone, but no undo takes them back.
    fn change_kind(&self) -> tilo_actions::context::ChangeKind {
        match self {
            CalendarAction::AdoptRemoteEvent(_)
            | CalendarAction::AdoptRemoteTask(_)
            | CalendarAction::DropRemoteItem { .. } => tilo_actions::context::ChangeKind::External,
            CalendarAction::SetRemoteBinding { .. }
            | CalendarAction::AcknowledgeRemotePush { .. }
            | CalendarAction::AcknowledgeRemoteDelete { .. }
            | CalendarAction::SetSyncToken { .. } => {
                tilo_actions::context::ChangeKind::Housekeeping
            }
            _ => tilo_actions::context::ChangeKind::Content,
        }
    }

    /// Outside-consequence undo: creating a remote event is undone by a
    /// remote delete (`del_outbox`); undoing that delete re-upserts and
    /// re-queues the push. Computed before `apply` so a delete can still
    /// read the event it removes. Pure local CRDT edits leave this as `None`.
    fn inverse_before(&self, doc: &Doc) -> Option<Self> {
        match self {
            CalendarAction::UpsertEvent(data) => {
                let event = data.to_event()?;
                let calendar = event.calendar_id.clone()?;
                Some(CalendarAction::DeleteEvent {
                    id: event.id.to_string(),
                    remote_delete: Some(calendar),
                })
            }
            CalendarAction::PatchEvent { id, patch } if patch.enqueue_remote == Some(true) => {
                // Filing an existing event onto a remote calendar: after Sync
                // has PUT it, undo must queue a CalDAV delete.
                let event_id = id.parse::<Id>().ok()?;
                let event = CalDoc::read_model(doc).event(event_id)?.clone();
                let calendar = patch
                    .calendar_id
                    .clone()
                    .filter(|value| !value.is_empty())
                    .or_else(|| event.calendar_id.clone())?;
                Some(CalendarAction::DeleteEvent {
                    id: id.clone(),
                    remote_delete: Some(calendar),
                })
            }
            CalendarAction::DeleteEvent {
                id,
                remote_delete: Some(calendar),
            } => {
                let event_id = id.parse::<Id>().ok()?;
                let event = CalDoc::read_model(doc).event(event_id)?.clone();
                if event.calendar_id.as_deref() != Some(calendar.as_str()) {
                    return None;
                }
                // UpsertEvent of a remote event re-queues the push in apply.
                Some(CalendarAction::UpsertEvent(EventData::from_event(&event)))
            }
            _ => None,
        }
    }

    fn inverse_encoded(&self, doc: &Doc) -> Option<EncodedAction> {
        self.inverse_before(doc)
            .and_then(|inverse| EncodedAction::encode(CALENDAR_ACTION_TYPE, &inverse).ok())
    }
}

/// The import of the calendar file `source`: its events, each under an id of
/// its own, and its tasks under the ids their UIDs name, so a task imported
/// again replaces itself. `None` when the file holds no supported item.
pub fn import_action(source: &str) -> Option<CalendarAction> {
    let (events, tasks) = crate::ics::parse(source);
    let events: Vec<_> = events
        .into_iter()
        .map(|mut event| {
            event.id = Uuid::new_v4();
            EventData::from_event(&event)
        })
        .collect();
    let tasks: Vec<_> = tasks.iter().map(TaskData::from_task).collect();
    (!events.is_empty() || !tasks.is_empty()).then_some(CalendarAction::ImportIcs { events, tasks })
}

/// Register the calendar's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<CalendarAction>(CALENDAR_ACTION_TYPE);
}

/// Encode any [`CalendarAction`] into the wire form the palette and the on-screen
/// affordances both dispatch. The single point that turns a calendar action into
/// a replayable envelope.
///
/// # Panics
/// Never panics: falls back to an empty-payload envelope rather than crashing
/// the caller if `action` somehow fails to serialize.
pub fn encode(action: &CalendarAction) -> EncodedAction {
    EncodedAction::encode(CALENDAR_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: CALENDAR_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

fn apply_set_remote_binding(
    doc: &mut Doc,
    id: &str,
    calendar_id: &str,
    href: &str,
    etag: &str,
) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("binding id is invalid"))?;
    if calendar_id.is_empty()
        || calendar_id.len() > 512
        || href.is_empty()
        || href.len() > 2048
        || etag.len() > 512
    {
        return Err(refused("remote binding is invalid"));
    }
    CalDoc::set_remote_binding(
        doc,
        id,
        calendar_id,
        href,
        (!etag.is_empty()).then_some(etag),
    );
    Ok(())
}

fn apply_adopt_remote_event(doc: &mut Doc, data: &EventData) -> Result<(), ActionError> {
    let event = data
        .to_event()
        .ok_or_else(|| refused("remote event is invalid"))?;
    CalDoc::update_event(doc, &event);
    Ok(())
}

fn apply_adopt_remote_task(doc: &mut Doc, data: &TaskData) -> Result<(), ActionError> {
    let task = data
        .to_task()
        .ok_or_else(|| refused("remote task is invalid"))?;
    CalDoc::update_task(doc, &task);
    Ok(())
}

fn apply_drop_remote_item(doc: &mut Doc, id: &str) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("remote item id is invalid"))?;
    CalDoc::delete_event(doc, id);
    CalDoc::delete_task(doc, id);
    CalDoc::remove_remote_binding(doc, id);
    CalDoc::dequeue_push(doc, id);
    Ok(())
}

fn apply_acknowledge_remote_push(doc: &mut Doc, id: &str) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("remote item id is invalid"))?;
    CalDoc::dequeue_push(doc, id);
    Ok(())
}

fn apply_acknowledge_remote_delete(doc: &mut Doc, id: &str) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("remote item id is invalid"))?;
    CalDoc::dequeue_delete(doc, id);
    CalDoc::remove_remote_binding(doc, id);
    Ok(())
}

fn apply_set_sync_token(doc: &mut Doc, calendar_id: &str, token: &str) -> Result<(), ActionError> {
    if calendar_id.is_empty() || calendar_id.len() > 512 || token.len() > 4096 {
        return Err(refused("sync token is invalid"));
    }
    CalDoc::set_sync_token(doc, calendar_id, token);
    Ok(())
}

/// Create or replace an event (new event, any field edit, or a move).
pub fn upsert_event_action(e: &Event) -> CalendarAction {
    CalendarAction::UpsertEvent(EventData::from_event(e))
}

/// File an event on a calendar and update the push queue accordingly.
pub fn set_event_calendar_action(e: &Event, enqueue: bool) -> CalendarAction {
    CalendarAction::PatchEvent {
        id: e.id.to_string(),
        patch: EventPatch {
            calendar_id: Some(e.calendar_id.clone().unwrap_or_default()),
            color: Some(e.color.index()),
            enqueue_remote: Some(enqueue),
            ..EventPatch::default()
        },
    }
}

/// Apply a sparse event mutation. Callers that have an in-memory before/after
/// pair should use [`EventPatch::between`] so unchanged registers never become
/// accidental last-writer-wins writes.
pub fn patch_event_action(id: Id, patch: EventPatch) -> CalendarAction {
    CalendarAction::PatchEvent {
        id: id.to_string(),
        patch,
    }
}

/// Delete an event; `remote_delete` names the source calendar when a `CalDAV`
/// delete should be queued too.
pub fn delete_event_action(id: impl Into<String>, remote_delete: Option<String>) -> CalendarAction {
    CalendarAction::DeleteEvent {
        id: id.into(),
        remote_delete,
    }
}

/// Create or replace a task.
pub fn upsert_task_action(t: &Task) -> CalendarAction {
    CalendarAction::UpsertTask(TaskData::from_task(t))
}

/// Apply a sparse task mutation without rewriting unrelated task registers.
pub fn patch_task_action(id: Id, patch: TaskPatch) -> CalendarAction {
    CalendarAction::PatchTask {
        id: id.to_string(),
        patch,
    }
}

/// Set a task's completion to a deterministic absolute value.
pub fn set_task_done_action(id: impl Into<String>, done: bool) -> CalendarAction {
    CalendarAction::SetTaskDone {
        id: id.into(),
        done,
    }
}

/// Delete a task and its subtasks.
pub fn delete_task_action(id: impl Into<String>, subtasks: Vec<String>) -> CalendarAction {
    CalendarAction::DeleteTask {
        id: id.into(),
        subtasks,
    }
}

// ===========================================================================
// Calendar collections
// ===========================================================================

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
mod tests {
    use super::*;
    use crate::app::CalendarApp;
    use automerge::{AutoCommit, ROOT};
    use tilo_actions::LocalTransport;
    use tilo_actions::Resource;
    use tilo_automerge::ApplyIncoming;
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        ActionRegistry::with(register)
    }

    fn find_kind<'a>(resources: &'a [Resource], kind: &str) -> Option<&'a Resource> {
        for resource in resources {
            if resource.kind == kind {
                return Some(resource);
            }
            if let Some(found) = find_kind(&resource.children, kind) {
                return Some(found);
            }
        }
        None
    }

    /// A demo calendar synced into a fresh document, plus the app that owns it.
    fn seeded() -> (CalendarApp, AutoCommit) {
        let mut app = CalendarApp::new_demo();
        let doc = crate::app::demo_document(&app);
        app.sync(&doc, &ROOT);
        (app, doc)
    }

    /// The calendar as it publishes `doc`: its own actions, and a page of its
    /// items with theirs.
    fn published(doc: &AutoCommit) -> Vec<Resource> {
        vec![ui::tilo_resource::publish::<crate::resource::Calendar>(
            "calendar.calendar",
            "Calendar",
            &CalDoc::read_model(doc),
        )]
    }

    /// The item `id` as the calendar resolves it, published or not.
    fn resolve(doc: &AutoCommit, id: &str) -> Option<Resource> {
        let model = CalDoc::read_model(doc);
        <crate::resource::Calendar as ui::tilo_resource::Resource>::child(&model, id)
            .or_else(|| find_id(&published(doc), id))
    }

    fn find_id(resources: &[Resource], id: &str) -> Option<Resource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource.clone())
            } else {
                find_id(&resource.children, id)
            }
        })
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn the_calendar_publishes_a_bounded_page_and_resolves_every_item_by_id() {
        // Case `ctrl_k_publishes_the_calendar_tree`:
        // Ctrl+K on the calendar yields the tree: the calendar with a "New event"
        // action, an `calendar.event` child per event exporting open/rename/delete,
        // and a `calendar.task` child per task exporting open/toggle/delete.
        // Drives the real resource hook.
        {
            let (_, doc) = seeded();
            let resources = published(&doc);
            assert_eq!(resources.len(), 1, "one calendar root");
            let cal = &resources[0];
            assert_eq!(cal.kind, "calendar.calendar");
            let cal_actions: Vec<&str> = cal.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(cal_actions.contains(&"new-event"), "{cal_actions:?}");
            assert!(cal_actions.contains(&"new-project"), "{cal_actions:?}");

            let project = find_kind(&cal.children, "calendar.project").expect("a project resource");
            let project_actions: Vec<&str> =
                project.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(project_actions.contains(&"open"), "{project_actions:?}");
            assert!(project_actions.contains(&"edit"), "{project_actions:?}");
            assert!(
                project_actions.contains(&"set-color"),
                "{project_actions:?}"
            );
            assert!(project_actions.contains(&"delete"), "{project_actions:?}");

            let event = find_kind(&cal.children, "calendar.event").expect("an event resource");
            let ev_actions: Vec<&str> = event.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(ev_actions.contains(&"open"), "{ev_actions:?}");
            assert!(ev_actions.contains(&"edit"), "{ev_actions:?}");
            assert!(ev_actions.contains(&"delete"), "{ev_actions:?}");

            let task = find_kind(&cal.children, "calendar.task").expect("a task resource");
            let tk_actions: Vec<&str> = task.actions.iter().map(|a| a.id.as_str()).collect();
            assert!(tk_actions.contains(&"open"), "{tk_actions:?}");
            assert!(tk_actions.contains(&"edit"), "{tk_actions:?}");
            assert!(tk_actions.contains(&"set-due"), "{tk_actions:?}");
            assert!(tk_actions.contains(&"set-priority"), "{tk_actions:?}");
            assert!(tk_actions.contains(&"move-to-project"), "{tk_actions:?}");
            assert!(tk_actions.contains(&"toggle"), "{tk_actions:?}");
            assert!(tk_actions.contains(&"delete"), "{tk_actions:?}");

            // The full flattened tree exports every action — including the task
            // toggle/delete that sort after the events. The palette's Actions group
            // is display-capped, so reachability is asserted on the flatten and only
            // the leading rows on the palette itself.
            let flat: Vec<&str> = tilo_actions::flatten(&resources)
                .iter()
                .map(|f| f.action.label.as_str())
                .collect();
            assert!(flat.contains(&"New event"), "{flat:?}");
            assert!(flat.contains(&"Delete event"), "{flat:?}");
            assert!(flat.contains(&"Delete task"), "{flat:?}");
            assert!(
                flat.iter()
                    .any(|l| *l == "Mark done" || *l == "Mark not done"),
                "a task toggle is exported: {flat:?}"
            );
        }
        // Case `large_resource_sets_are_capped_but_late_ids_resolve_lazily_and_stale_ids_refuse`.
        {
            let (_, mut doc) = seeded();
            let template = CalDoc::read_model(&doc).events[0].clone();
            let mut late_id = template.id;
            for offset in 0..(crate::tree::PAGE + 2) {
                let mut event = template.clone();
                event.id = Uuid::from_u128(10_000 + offset as u128);
                event.title = format!("lazy resource event {offset}");
                late_id = event.id;
                CalendarAction::UpsertEvent(EventData::from_event(&event))
                    .apply(&mut doc)
                    .expect("valid event is accepted");
            }

            let root = published(&doc).remove(0);
            let items = root
                .children
                .iter()
                .filter(|child| child.kind.starts_with("calendar."))
                .count();
            assert!(
                items <= crate::tree::PAGE,
                "the published page stays bounded"
            );
            let late = event_resource_id(late_id);
            assert!(
                find_id(&root.children, &late).is_none(),
                "the late event is not paged"
            );
            let resolved = resolve(&doc, &late).expect("a current late event resolves on demand");
            assert_eq!(resolved.id.as_str(), late);
            assert!(resolved.actions.iter().any(|action| action.id == "open"));

            CalendarAction::DeleteEvent {
                id: late_id.to_string(),
                remote_delete: None,
            }
            .apply(&mut doc)
            .expect("delete is accepted");
            assert!(
                resolve(&doc, &late).is_none(),
                "a deleted stable id is refused rather than redirected"
            );
        }
        // Case `deep_resource_actions_resolve_by_exact_id`.
        {
            fn collect_deep<'a>(resource: &'a Resource, found: &mut Vec<&'a Resource>) {
                if matches!(
                    resource.kind.as_str(),
                    "calendar.reminder" | "calendar.attendee" | "calendar.occurrence"
                ) || resource.id.as_str().ends_with("/notes")
                {
                    found.push(resource);
                }
                for child in &resource.children {
                    collect_deep(child, found);
                }
            }

            let (_, doc) = seeded();
            let root = published(&doc).remove(0);
            let mut deep = Vec::new();
            collect_deep(&root, &mut deep);
            for kind in [
                "calendar.reminder",
                "calendar.attendee",
                "calendar.occurrence",
            ] {
                assert!(
                    deep.iter().any(|resource| resource.kind == kind),
                    "the sample calendar publishes a {kind}"
                );
            }
            assert!(
                deep.iter()
                    .any(|resource| resource.id.as_str().ends_with("/notes"))
            );

            for resource in deep {
                let resolved = <crate::resource::Calendar as ui::tilo_resource::Resource>::child(
                    &CalDoc::read_model(&doc),
                    resource.id.as_str(),
                )
                .unwrap_or_else(|| panic!("{} resolves by id", resource.id.as_str()));
                assert_eq!(resolved.id, resource.id);
                for action in &resource.actions {
                    assert!(
                        resolved
                            .actions
                            .iter()
                            .any(|candidate| candidate.id == action.id),
                        "{} action {} stays reachable by exact resource id",
                        resource.id.as_str(),
                        action.id
                    );
                }
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn search_rows_and_project_actions_invoke_exports_preserving_relationships() {
        // Case `search_rows_invoke_exported_event_and_task_view_actions`:
        // A search hit invokes the exact `open` view operation the item
        // publishes, through the same door as the palette.
        {
            let (mut app, doc) = seeded();
            let resources = published(&doc);
            let model = CalDoc::read_model(&doc);
            let entries =
                <crate::resource::Calendar as ui::tilo_resource::Resource>::search(&model);
            assert!(
                entries
                    .iter()
                    .all(|entry| entry.action.as_ref().is_some_and(|address| {
                        resolve(&doc, &address.resource).is_some_and(|resource| {
                            resource.actions.iter().any(|a| a.id == address.action)
                        })
                    })),
                "every event, task and project hit resolves to its open action"
            );

            for (query, expected_label, expected_kind) in [
                ("bright smiles", "Dentist", "calendar.event"),
                ("renew passport", "Renew passport", "calendar.task"),
            ] {
                let entry = entries
                    .iter()
                    .find(|entry| {
                        entry.title == expected_label
                            && format!("{} {}", entry.title, entry.text)
                                .to_lowercase()
                                .contains(query)
                    })
                    .expect("the searchable content row is present");
                let address = entry.action.clone().expect("a hit invokes an action");
                assert!(address.resource.starts_with(expected_kind));
                let spec = resolve(&doc, &address.resource)
                    .and_then(|resource| {
                        resource
                            .actions
                            .into_iter()
                            .find(|action| action.id == address.action)
                    })
                    .expect("the hit's action is published");
                assert_eq!(spec.kind, tilo_actions::OperationKind::View);
                let encoded = spec
                    .build_args(std::iter::empty::<(String, tilo_actions::ArgValue)>())
                    .expect("open takes no arguments");
                let crate::resource::CalendarEffect::Open { item } =
                    <crate::resource::CalendarEffect as ui::tilo_resource::Effect>::from_payload(
                        &encoded.payload,
                    )
                    .expect("the hit opens through the calendar's effects")
                else {
                    panic!("a hit opens its item");
                };
                app.open_item(&item).expect("view operation succeeds");
                let id = address
                    .resource
                    .rsplit_once(':')
                    .and_then(|(_, id)| id.parse::<Id>().ok())
                    .expect("resource id ends in the model UUID");
                if expected_kind == "calendar.event" {
                    assert_eq!(app.selected_event(), Some(id), "exact event was selected");
                } else {
                    assert_eq!(app.selected_task(), Some(id), "exact task was selected");
                }
            }
            let _ = resources;
        }
        // Case `projects_are_searchable_and_project_actions_preserve_relationships`.
        {
            let (_, mut doc) = seeded();
            let resources = published(&doc);
            let model = CalDoc::read_model(&doc);
            let linked_id = model
                .projects
                .iter()
                .find(|project| {
                    model
                        .tasks
                        .iter()
                        .any(|task| task.project == Some(project.id))
                })
                .expect("demo has a linked project")
                .id
                .to_string();
            let project = resources[0]
                .children
                .iter()
                .find(|resource| {
                    resource.id.as_str() == project_resource_id(linked_id.parse().unwrap())
                })
                .expect("seeded project resource");
            let id = project
                .id
                .as_str()
                .trim_start_matches("calendar.project:")
                .parse::<Id>()
                .expect("project resource id");
            let name = CalDoc::read_model(&doc)
                .project(id)
                .expect("project")
                .name
                .clone();
            assert!(
                <crate::resource::Calendar as ui::tilo_resource::Resource>::search(&model)
                    .iter()
                    .any(|entry| entry.title == name
                        && entry.action == Some(open_project_address(id)))
            );

            let rename = project
                .actions
                .iter()
                .find(|action| action.id == "edit")
                .expect("rename action");
            let encoded = rename
                .build_args([(
                    "name".to_string(),
                    tilo_actions::ArgValue::Text("Critical path".to_string()),
                )])
                .expect("typed project rename");
            let decoded = registry().decode(&encoded).expect("calendar action");
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
                .expect("project rename dispatches");
            assert_eq!(
                CalDoc::read_model(&doc).project(id).expect("project").name,
                "Critical path"
            );

            let delete = CalendarAction::DeleteProject { id: id.to_string() }.apply(&mut doc);
            assert!(
                matches!(delete, Err(ActionError::Refused(_))),
                "a linked project is not silently detached"
            );
            assert!(CalDoc::read_model(&doc).project(id).is_some());
        }
        // Case `independent_project_fields_merge_and_history_keeps_the_project_identity`.
        {
            let (_app, mut first) = seeded();
            first.commit();
            let mut second = AutoCommit::load(&first.save()).expect("clone calendar document");
            let project = CalDoc::read_model(&first).projects[0].clone();

            CalendarAction::PatchProject {
                id: project.id.to_string(),
                patch: ProjectPatch {
                    name: Some("Renamed concurrently".into()),
                    ..ProjectPatch::default()
                },
            }
            .apply(&mut first)
            .expect("rename applies");
            CalendarAction::PatchProject {
                id: project.id.to_string(),
                patch: ProjectPatch {
                    color: Some(Palette::Violet.index()),
                    ..ProjectPatch::default()
                },
            }
            .apply(&mut second)
            .expect("colour applies");
            first.commit();
            second.commit();
            first
                .merge_incoming(&mut second)
                .expect("concurrent changes merge");

            let merged_model = CalDoc::read_model(&first);
            let merged = merged_model
                .project(project.id)
                .expect("project survives merge");
            assert_eq!(merged.name, "Renamed concurrently");
            assert_eq!(merged.color, Palette::Violet);

            let context = history_context(&CalendarAction::PatchProject {
                id: project.id.to_string(),
                patch: ProjectPatch {
                    name: Some("Again".into()),
                    ..ProjectPatch::default()
                },
            });
            assert_eq!(
                context.resource.expect("project history resource").id,
                project_resource_id(project.id)
            );
        }
    }

    fn first_event(doc: &AutoCommit) -> crate::model::Event {
        CalDoc::read_model(doc).events[0].clone()
    }

    fn event_id_of(res: &Resource) -> &str {
        res.id.as_str().trim_start_matches("calendar.event:")
    }

    ui::action_contract_tests! {
        seed: seeded().1,
        registry: registry(),
        resources: |doc| published(doc),
        // The neutral action list is deliberately display-capped; filtering
        // must still bring every deeper event action into the same palette.
        palette: ["New event", "New task", "Set reminder", "Delete event"],
        replay: [
            // Completion is an absolute value, so replay does not depend on the
            // receiving replica's current task state.
            set_task_done => |doc| {
                let task = CalDoc::read_model(doc)
                    .tasks
                    .into_iter()
                    .find(|t| !t.done)
                    .unwrap();
                encode(&set_task_done_action(task.id.to_string(), true))
            },
            // The create/edit/move path, not just the toggle.
            upsert_event => |doc| {
                let mut event = first_event(doc);
                event.title = "Renamed via action".into();
                encode(&upsert_event_action(&event))
            },
            edit_notes => |doc| encode(&CalendarAction::EditNotes {
                owner: NoteOwner::Event {
                    id: first_event(doc).id.to_string(),
                },
                edit: RichTextAction::AppendParagraph {
                    text: "Résumé ✓".to_string(),
                },
            }),
        ],
        undo: [
            delete_event => |doc| encode(&delete_event_action(first_event(doc).id.to_string(), None)),
        ],
        encodes: [
            // A synced event's delete queues its remote delete, as the trash
            // button's does.
            delete_event => ("calendar.event", "delete", |doc, res| {
                let id = event_id_of(res);
                let remote = CalDoc::read_model(doc)
                    .events
                    .iter()
                    .find(|e| e.id.to_string() == id)
                    .and_then(|e| e.calendar_id.clone());
                encode(&delete_event_action(id.to_string(), remote))
            }),
            // A field-level patch, not a stale whole-event snapshot that could
            // overwrite another collaborator's edit.
            rename_event => ("calendar.event", "edit", arg: "New title", |_, res| {
                encode(&patch_event_action(
                    event_id_of(res).parse().unwrap(),
                    EventPatch {
                        title: Some("New title".into()),
                        ..EventPatch::default()
                    },
                ))
            }),
        ],
    }

    #[test]
    fn invalid_and_hostile_mutations_are_refused_atomically() {
        // Case `rename_event_refuses_a_blank_title`.
        {
            let (_, doc) = seeded();
            let resources = published(&doc);
            let event = find_kind(&resources[0].children, "calendar.event").expect("an event");
            let edit_spec = event.actions.iter().find(|a| a.id == "edit").unwrap();
            assert_eq!(
                edit_spec.build_text("   ").unwrap().type_id,
                tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
                "rename refuses a whitespace-only title"
            );
        }
        // Case `upserts_reject_unknown_project_relationships`.
        {
            let (_app, mut doc) = seeded();
            let mut event = EventData::from_event(&CalDoc::read_model(&doc).events[0]);
            event.id = Id::new_v4().to_string();
            event.project = Id::new_v4().to_string();
            let authored_id = event.id.clone();
            CalendarAction::UpsertEvent(event).apply(&mut doc).unwrap();
            assert!(
                CalDoc::read_model(&doc)
                    .events
                    .iter()
                    .all(|candidate| candidate.id.to_string() != authored_id),
                "a valid UUID is not enough: a project relationship must resolve in the document"
            );
        }
        // Case `hostile_patch_is_refused_atomically_before_remote_queueing`.
        {
            let (_app, mut doc) = seeded();
            let event = CalDoc::read_model(&doc).events[0].clone();
            let result = CalendarAction::PatchEvent {
                id: event.id.to_string(),
                patch: EventPatch {
                    end: Some(
                        (event.start - chrono::Duration::minutes(1))
                            .format(DT_FMT)
                            .to_string(),
                    ),
                    enqueue_remote: Some(true),
                    ..EventPatch::default()
                },
            }
            .apply(&mut doc);
            assert!(matches!(result, Err(ActionError::Refused(_))));
            let after = CalDoc::read_model(&doc).event(event.id).unwrap().clone();
            assert_eq!(after.start, event.start);
            assert_eq!(after.end, event.end);
            assert!(!CalDoc::outbox(&doc).contains(&event.id));
        }
        // Case `hostile_task_delete_cannot_name_an_unrelated_task`.
        {
            let (_app, mut doc) = seeded();
            let model = CalDoc::read_model(&doc);
            let parent = model.tasks[0].id;
            let unrelated = model
                .tasks
                .iter()
                .find(|task| task.parent != Some(parent) && task.id != parent)
                .expect("demo has an unrelated task")
                .id;
            let result = CalendarAction::DeleteTask {
                id: parent.to_string(),
                subtasks: vec![unrelated.to_string()],
            }
            .apply(&mut doc);
            assert!(matches!(result, Err(ActionError::Refused(_))));
            assert!(CalDoc::read_model(&doc).task(unrelated).is_some());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn replicated_edits_converge_and_imports_replay_deterministically() {
        // Case `invite_recurrence_and_timezone_actions_converge_across_divergent_replicas`.
        {
            let (_app, mut first) = seeded();
            let event_id = CalDoc::read_model(&first).events[0].id;
            let attendee_id = Uuid::new_v4();
            CalendarAction::UpsertAttendee {
                event_id: event_id.to_string(),
                attendee: AttendeeData {
                    id: attendee_id.to_string(),
                    name: "Sam".into(),
                    email: "sam@example.com".into(),
                    status: "NEEDS-ACTION".into(),
                },
            }
            .apply(&mut first)
            .expect("invite applies");
            first.commit();
            let bytes = first.save();
            let mut second = AutoCommit::load(&bytes).expect("second replica");

            CalendarAction::PatchAttendee {
                event_id: event_id.to_string(),
                attendee_id: attendee_id.to_string(),
                patch: AttendeePatch {
                    status: Some("ACCEPTED".into()),
                    ..AttendeePatch::default()
                },
            }
            .apply(&mut first)
            .expect("response applies");
            CalendarAction::PatchEvent {
                id: event_id.to_string(),
                patch: EventPatch {
                    timezone: Some("Europe/Berlin".into()),
                    recurrence_interval: Some(2),
                    recurrence_count: Some(7),
                    ..EventPatch::default()
                },
            }
            .apply(&mut second)
            .expect("series edit applies");
            CalendarAction::SetRecurrenceException {
                event_id: event_id.to_string(),
                date: "2026-06-17".into(),
                excluded: true,
            }
            .apply(&mut second)
            .expect("exception applies");
            first.commit();
            second.commit();
            let mut first_peer = AutoCommit::load(&first.save()).expect("first peer");
            let mut second_peer = AutoCommit::load(&second.save()).expect("second peer");
            first
                .merge_incoming(&mut second_peer)
                .expect("merge second into first");
            second
                .merge_incoming(&mut first_peer)
                .expect("merge first into second");

            let left = CalDoc::read_model(&first).event(event_id).cloned().unwrap();
            let right = CalDoc::read_model(&second)
                .event(event_id)
                .cloned()
                .unwrap();
            assert_eq!(left.timezone, right.timezone);
            assert_eq!(left.recurrence_interval, 2);
            assert_eq!(left.recurrence_count, Some(7));
            assert_eq!(left.recurrence_exdates, right.recurrence_exdates);
            assert_eq!(left.attendees, right.attendees);
            assert_eq!(left.attendees[0].status, InviteStatus::Accepted);
        }
        // Case `attendee_delete_tombstone_wins_over_stale_update`.
        {
            let (_app, mut first) = seeded();
            let event_id = CalDoc::read_model(&first).events[0].id;
            let attendee_id = Uuid::new_v4();
            CalendarAction::UpsertAttendee {
                event_id: event_id.to_string(),
                attendee: AttendeeData {
                    id: attendee_id.to_string(),
                    name: "Sam".into(),
                    email: "sam@example.com".into(),
                    status: "NEEDS-ACTION".into(),
                },
            }
            .apply(&mut first)
            .unwrap();
            first.commit();
            let mut second = AutoCommit::load(&first.save()).unwrap();
            CalendarAction::DeleteAttendee {
                event_id: event_id.to_string(),
                attendee_id: attendee_id.to_string(),
            }
            .apply(&mut first)
            .unwrap();
            CalendarAction::PatchAttendee {
                event_id: event_id.to_string(),
                attendee_id: attendee_id.to_string(),
                patch: AttendeePatch {
                    status: Some("ACCEPTED".into()),
                    ..AttendeePatch::default()
                },
            }
            .apply(&mut second)
            .unwrap();
            first.commit();
            second.commit();
            first.merge_incoming(&mut second).unwrap();
            assert!(
                CalDoc::read_model(&first)
                    .event(event_id)
                    .unwrap()
                    .attendees
                    .is_empty()
            );
        }
        // Case `import_history_points_at_the_actual_calendar_root`.
        {
            let context = history_context(&CalendarAction::ImportIcs {
                events: Vec::new(),
                tasks: Vec::new(),
            });
            assert_eq!(
                context.resource.expect("root resource").id,
                "calendar.calendar"
            );
        }
        // Case `import_batch_replays_as_one_deterministic_action`.
        {
            let (_app, seeded_doc) = seeded();
            let mut a = seeded_doc;
            a.commit();
            let bytes = a.save();
            let mut b = AutoCommit::load(&bytes).expect("same base");
            let event = EventData {
                id: Uuid::new_v4().to_string(),
                title: "Imported planning".into(),
                start: "2026-09-06T09:00".into(),
                end: "2026-09-06T10:00".into(),
                all_day: false,
                location: String::new(),
                color: Palette::Indigo.index(),
                rrule: String::new(),
                recurrence_interval: 1,
                recurrence_until: String::new(),
                recurrence_count: 0,
                recurrence_exdates: Vec::new(),
                occurrence_overrides: Vec::new(),
                timezone: "floating".into(),
                organizer: String::new(),
                attendees: Vec::new(),
                reminder: -1,
                project: String::new(),
                calendar_id: String::new(),
            };
            let action = CalendarAction::ImportIcs {
                events: vec![event.clone()],
                tasks: Vec::new(),
            };
            let encoded = encode(&action);
            let envelope =
                tilo_actions::apply_local(&mut a, &action, encoded).expect("apply import");
            tilo_actions::replay(&mut b, &envelope, &registry()).expect("replay import");
            assert_eq!(
                a.get_heads(),
                b.get_heads(),
                "one import payload converges exactly"
            );
            assert!(
                CalDoc::read_model(&a)
                    .events
                    .iter()
                    .any(|candidate| candidate.id.to_string() == event.id),
                "the captured item landed"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn event_and_task_actions_create_and_patch_typed_strict_fields() {
        // Case `new_event_action_creates_an_event`:
        // Dispatching the "New event" prompted action creates an event in the
        // document with the committed title.
        {
            let (_, mut doc) = seeded();
            let resources = published(&doc);
            let spec = resources[0]
                .actions
                .iter()
                .find(|a| a.id == "new-event")
                .unwrap();
            let before = CalDoc::read_model(&doc).events.len();

            let encoded = spec.build_text("Coffee with Dana").unwrap();
            let decoded = registry().decode(&encoded).unwrap();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

            let model = CalDoc::read_model(&doc);
            assert_eq!(model.events.len(), before + 1, "the action added an event");
            assert!(
                model.events.iter().any(|e| e.title == "Coffee with Dana"),
                "the created event carries the committed title"
            );
        }
        // Case `event_field_actions_are_typed_strict_and_replayable`:
        // The event editor's full state is not trapped behind pointer-only
        // popovers: agents receive strict typed controls that produce the same
        // replayable field patches as direct editing.
        {
            let (_, mut doc) = seeded();
            let resources = published(&doc);
            let event = find_kind(&resources, "calendar.event").expect("seeded event resource");
            let id = event
                .id
                .as_str()
                .trim_start_matches("calendar.event:")
                .parse::<Id>()
                .expect("event resource id");

            let location = event
                .actions
                .iter()
                .find(|action| action.id == "set-location")
                .expect("location action");
            let encoded = location
                .build_args([(
                    "location".to_string(),
                    tilo_actions::ArgValue::Text("Studio 3 · https://meet.example".to_string()),
                )])
                .expect("well-typed location request");
            let decoded = registry().decode(&encoded).expect("calendar action");
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
                .expect("location action dispatches");
            assert_eq!(
                CalDoc::read_model(&doc).event(id).expect("event").location,
                "Studio 3 · https://meet.example"
            );

            let repeat = event
                .actions
                .iter()
                .find(|action| action.id == "set-repeat")
                .expect("repeat action");
            assert!(
                repeat
                    .build_args([(
                        "repeat".to_string(),
                        tilo_actions::ArgValue::Text("every-other-tuesday".to_string()),
                    )])
                    .is_err(),
                "the declared choices reject an invented recurrence rather than silently defaulting"
            );
            let encoded = repeat
                .build_args([(
                    "repeat".to_string(),
                    tilo_actions::ArgValue::Text("weekly".to_string()),
                )])
                .expect("declared recurrence is accepted");
            let decoded = registry().decode(&encoded).expect("calendar action");
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
                .expect("repeat action dispatches");
            let updated = CalDoc::read_model(&doc).event(id).expect("event").clone();
            assert_eq!(updated.recurrence, Recurrence::Weekly);
            assert_eq!(
                updated.location, "Studio 3 · https://meet.example",
                "a recurrence patch did not rewrite an independently-authored location"
            );

            let project = CalDoc::read_model(&doc).projects[0].id;
            let move_project = event
                .actions
                .iter()
                .find(|action| action.id == "move-to-project")
                .expect("typed project resource action");
            let encoded = move_project
                .build_args([(
                    "project".to_string(),
                    tilo_actions::ArgValue::Resource(project_resource_id(project)),
                )])
                .expect("canonical project resource address is accepted");
            let decoded = registry().decode(&encoded).expect("calendar action");
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
                .expect("current project remains authoritatively valid");
            assert_eq!(
                CalDoc::read_model(&doc).event(id).unwrap().project,
                Some(project)
            );

            let move_calendar = event
                .actions
                .iter()
                .find(|action| action.id == "move-to-calendar")
                .expect("typed calendar resource action");
            let encoded = move_calendar
                .build_args([(
                    "calendar".to_string(),
                    tilo_actions::ArgValue::Resource(calendar_resource_id("local")),
                )])
                .expect("canonical local collection is accepted");
            let decoded = registry().decode(&encoded).expect("calendar action");
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
                .expect("local calendar move dispatches");
            assert!(
                CalDoc::read_model(&doc)
                    .event(id)
                    .unwrap()
                    .calendar_id
                    .is_none()
            );
        }
        // Case `new_task_action_creates_a_collaborative_task`.
        {
            let (_, mut doc) = seeded();
            let resources = published(&doc);
            let spec = resources[0]
                .actions
                .iter()
                .find(|action| action.id == "new-task")
                .expect("new-task action");
            let before = CalDoc::read_model(&doc).tasks.len();
            let encoded = spec.build_text("Book dentist appointment").unwrap();
            let decoded = registry().decode(&encoded).expect("calendar action");
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
                .expect("new task dispatches");
            let model = CalDoc::read_model(&doc);
            assert_eq!(model.tasks.len(), before + 1);
            assert!(
                model
                    .tasks
                    .iter()
                    .any(|task| task.title == "Book dentist appointment" && !task.done)
            );
            let blank = spec.build_text("\t ").unwrap();
            assert_eq!(
                blank.type_id,
                tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
                "new task refuses a whitespace-only title"
            );
        }
        // Case `task_field_actions_are_sparse_and_reject_hostile_priority_values`.
        {
            let (_, mut doc) = seeded();
            let resources = published(&doc);
            let task = find_kind(&resources, "calendar.task").expect("seeded task resource");
            let id = task
                .id
                .as_str()
                .trim_start_matches("calendar.task:")
                .parse::<Id>()
                .expect("task resource id");
            let original_title = CalDoc::read_model(&doc)
                .task(id)
                .expect("task")
                .title
                .clone();
            let priority = task
                .actions
                .iter()
                .find(|action| action.id == "set-priority")
                .expect("priority action");
            let encoded = priority
                .build_args([(
                    "priority".to_string(),
                    tilo_actions::ArgValue::Text("p1".to_string()),
                )])
                .expect("declared priority is accepted");
            let decoded = registry().decode(&encoded).expect("calendar action");
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
                .expect("priority action dispatches");
            let changed = CalDoc::read_model(&doc).task(id).expect("task").clone();
            assert_eq!(changed.priority, Priority::High);
            assert_eq!(changed.title, original_title, "priority patch kept title");

            // A peer is allowed to send a serialized action directly, so validate
            // again at the document boundary instead of trusting the UI choices.
            CalendarAction::PatchTask {
                id: id.to_string(),
                patch: TaskPatch {
                    priority: Some(200),
                    ..TaskPatch::default()
                },
            }
            .apply(&mut doc)
            .expect_err("malformed priority is refused before it can report success");
            assert_eq!(
                CalDoc::read_model(&doc).task(id).expect("task").priority,
                Priority::High,
                "out-of-range priority was not persisted"
            );
        }
    }

    /// Creating a remote-calendar event queues a push; after that push is
    /// drained (as the upload drain does), undo must run the inverse document
    /// action through `apply` so a `CalDAV` delete is queued. Inverse Automerge
    /// patches alone leave the server copy to be resurrected by a later pull.
    #[test]
    fn undo_of_remote_event_create_queues_caldav_delete_through_apply() {
        let remote = "work/team".to_owned();
        let data = EventData {
            id: Uuid::new_v4().to_string(),
            title: "Standup".into(),
            start: "2026-09-08T09:00".into(),
            end: "2026-09-08T09:30".into(),
            all_day: false,
            location: String::new(),
            color: Palette::Indigo.index(),
            rrule: String::new(),
            recurrence_interval: 1,
            recurrence_until: String::new(),
            recurrence_count: 0,
            recurrence_exdates: Vec::new(),
            occurrence_overrides: Vec::new(),
            timezone: "floating".into(),
            organizer: String::new(),
            attendees: Vec::new(),
            reminder: -1,
            project: String::new(),
            calendar_id: remote.clone(),
        };
        let id = data.id.parse::<Id>().expect("id");

        let mut doc = AutoCommit::new();
        let create = CalendarAction::UpsertEvent(data.clone());
        let undo_create = create
            .inverse_before(&doc)
            .expect("remote upsert names a compensating delete");
        assert!(
            matches!(
                &undo_create,
                CalendarAction::DeleteEvent {
                    remote_delete: Some(cal),
                    ..
                } if cal == &remote
            ),
            "inverse of a remote create is a remote delete, not a silent CRDT revert"
        );
        create.apply(&mut doc).expect("create applies");
        assert!(
            CalDoc::outbox(&doc).contains(&id),
            "creating a remote event records the push outbox entry"
        );
        // The upload drain acknowledged the push after a successful PUT.
        CalDoc::dequeue_push(&mut doc, id);
        assert!(CalDoc::delete_outbox(&doc).is_empty());

        // Undo through apply (not inverse patches).
        undo_create.apply(&mut doc).expect("inverse delete applies");
        assert!(CalDoc::read_model(&doc).event(id).is_none());
        assert!(
            CalDoc::delete_outbox(&doc)
                .iter()
                .any(|(cal, uid)| cal == &remote && uid == &id),
            "undo queued a CalDAV delete; patch-only undo would leave the server copy"
        );

        // Undoing that delete restores the event and re-queues the upload.
        let mut live = AutoCommit::new();
        CalendarAction::UpsertEvent(data.clone())
            .apply(&mut live)
            .unwrap();
        let delete = CalendarAction::DeleteEvent {
            id: data.id,
            remote_delete: Some(remote),
        };
        let undo_delete = delete
            .inverse_before(&live)
            .expect("remote delete names a restoring upsert");
        delete.apply(&mut live).unwrap();
        CalDoc::dequeue_delete(&mut live, id); // the delete drain acknowledged it
        undo_delete.apply(&mut live).expect("restore applies");
        assert!(CalDoc::read_model(&live).event(id).is_some());
        assert!(
            CalDoc::outbox(&live).contains(&id),
            "undoing a remote delete re-queues the upload"
        );
    }

    #[test]
    fn a_remote_binding_is_a_calendar_action() {
        let mut doc = automerge::AutoCommit::new();
        let id = uuid::Uuid::new_v4();
        CalendarAction::SetRemoteBinding {
            id: id.to_string(),
            calendar_id: "work/default".into(),
            href: "/calendars/work/event.ics".into(),
            etag: "\"v1\"".into(),
        }
        .apply(&mut doc)
        .unwrap();
        let binding = CalDoc::remote_binding(&doc, id).expect("binding");
        assert_eq!(binding.0, "work/default");
        assert_eq!(binding.1, "/calendars/work/event.ics");
        assert_eq!(binding.2.as_deref(), Some("\"v1\""));
        assert!(
            CalendarAction::SetRemoteBinding {
                id: id.to_string(),
                calendar_id: String::new(),
                href: "/calendars/work/event.ics".into(),
                etag: String::new(),
            }
            .apply(&mut doc)
            .is_err()
        );
    }

    #[test]
    fn a_pulled_event_is_adopted_without_queueing_an_upload() {
        let id = uuid::Uuid::new_v4();
        let event = crate::model::Event {
            id,
            title: "Standup".into(),
            start: chrono::NaiveDateTime::parse_from_str("2026-09-19T09:00", "%Y-%m-%dT%H:%M")
                .unwrap(),
            end: chrono::NaiveDateTime::parse_from_str("2026-09-19T10:00", "%Y-%m-%dT%H:%M")
                .unwrap(),
            all_day: false,
            location: String::new(),
            color: crate::model::Palette::from_index(0),
            recurrence: crate::model::Recurrence::None,
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
            calendar_id: Some("work/default".into()),
        };
        let mut doc = automerge::AutoCommit::new();
        CalendarAction::AdoptRemoteEvent(EventData::from_event(&event))
            .apply(&mut doc)
            .unwrap();
        assert_eq!(
            CalDoc::read_model(&doc)
                .event(id)
                .map(|event| event.title.clone()),
            Some("Standup".into())
        );
        assert!(
            !CalDoc::outbox(&doc).contains(&id),
            "a pulled event must not be queued for upload"
        );
        CalendarAction::SetRemoteBinding {
            id: id.to_string(),
            calendar_id: "work/default".into(),
            href: "/calendars/work/standup.ics".into(),
            etag: "\"v1\"".into(),
        }
        .apply(&mut doc)
        .unwrap();
        CalendarAction::DropRemoteItem { id: id.to_string() }
            .apply(&mut doc)
            .unwrap();
        assert!(CalDoc::read_model(&doc).event(id).is_none());
        assert!(CalDoc::remote_binding(&doc, id).is_none());
        assert!(CalDoc::delete_outbox(&doc).is_empty());
        CalDoc::enqueue_push(&mut doc, id);
        CalendarAction::AcknowledgeRemotePush { id: id.to_string() }
            .apply(&mut doc)
            .unwrap();
        assert!(!CalDoc::outbox(&doc).contains(&id));
        CalDoc::enqueue_delete(&mut doc, "work/default", id);
        CalendarAction::SetRemoteBinding {
            id: id.to_string(),
            calendar_id: "work/default".into(),
            href: "/calendars/work/standup.ics".into(),
            etag: "\"v2\"".into(),
        }
        .apply(&mut doc)
        .unwrap();
        CalendarAction::AcknowledgeRemoteDelete { id: id.to_string() }
            .apply(&mut doc)
            .unwrap();
        assert!(CalDoc::delete_outbox(&doc).is_empty());
        assert!(CalDoc::remote_binding(&doc, id).is_none());
        CalendarAction::SetSyncToken {
            calendar_id: "work/default".into(),
            token: "sync-1".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            CalDoc::sync_tokens(&doc),
            vec![("work/default".into(), "sync-1".into())]
        );
    }
}
