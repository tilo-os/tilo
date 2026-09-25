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

use automerge::AutoCommit;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};
use ui::widget::BlockEditOp;
use uuid::Uuid;

use tilo_actions::{
    Action, ActionAddress, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, OperationKind, ParamType, PermissionRequirement, Resource,
    ResourceProvider, ResultSpec, RetryPolicy,
};

use crate::doc::CalDoc;
use crate::model::{
    Attendee, Event, Id, InviteStatus, OccurrenceOverride, Palette, Priority, Project, Recurrence,
    Task,
};

/// Registry `type_id` for every calendar action.
pub const CALENDAR_ACTION_TYPE: &str = "calendar.action";
pub const CALENDAR_OPERATION_TYPE: &str = "calendar.operation";

/// The root resource is intentionally a contextual snapshot, not a second
/// unbounded copy of Calendar's data. Search and direct invocation resolve any
/// omitted stable id through [`ResourceProvider::resource`] below.
pub(crate) const EAGER_RESOURCE_SNAPSHOT_LIMIT: usize = 96;
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

fn project_id_from_resource(address: &str) -> Option<Id> {
    address.strip_prefix("calendar.project:")?.parse().ok()
}

fn calendar_id_from_resource(address: &str) -> Option<&str> {
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

/// Canonical view action selecting one calendar collection.
pub fn open_calendar_address(id: &str) -> ActionAddress {
    ActionAddress::new(calendar_resource_id(id), "open")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalendarOperation {
    OpenEvent {
        id: String,
    },
    OpenOccurrence {
        event_id: String,
        original_date: String,
    },
    OpenTask {
        id: String,
    },
    OpenProject {
        id: String,
    },
    OpenCalendar {
        id: String,
    },
    ShowAllCalendars,
    ShowMonth,
    ShowWeek,
    ShowDay,
    ShowAgenda,
    /// Device-private visibility policy for one configured remote collection.
    /// This deliberately lives outside the shared event CRDT: hiding a work
    /// calendar is a per-person display choice, never a collaborative edit.
    SetCalendarVisible {
        id: String,
        visible: bool,
    },
    Sync,
    ExportIcs,
    /// Import the newest `.ics` file from the transfer directory. The effect
    /// captures parsed records into one [`CalendarAction`] before it reports
    /// success; its retry contract is therefore at-most-once.
    ImportIcs,
    /// Complete an explicitly queued local launch import after host admission.
    /// No path or source contents are carried in the operation envelope.
    FinalizeLaunchImport,
    /// Produce the exact typed invocation accepted by Mail's invitation-draft
    /// action. This prepares a reviewable draft; it never claims SMTP delivery.
    PrepareInvitationInMail {
        event_id: String,
        attendee_id: String,
    },
}

fn calendar_operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    operation: &CalendarOperation,
) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, CALENDAR_OPERATION_TYPE, operation)
        .unwrap_or_else(|_| {
            ActionSpec::immediate_encoded(
                id,
                label,
                ExecutionLocus::ClientLocal,
                encode_operation(operation),
            )
        });
    match operation {
        CalendarOperation::OpenEvent { .. }
        | CalendarOperation::OpenOccurrence { .. }
        | CalendarOperation::OpenTask { .. }
        | CalendarOperation::OpenProject { .. }
        | CalendarOperation::OpenCalendar { .. }
        | CalendarOperation::ShowAllCalendars
        | CalendarOperation::ShowMonth
        | CalendarOperation::ShowWeek
        | CalendarOperation::ShowDay
        | CalendarOperation::ShowAgenda
        | CalendarOperation::SetCalendarVisible { .. } => {
            spec.retry_policy(RetryPolicy::Idempotent)
        }
        CalendarOperation::Sync => spec
            .retry_policy(RetryPolicy::Idempotent)
            .execution(tilo_actions::ExecutionMode::Background { cancellable: false }),
        CalendarOperation::ExportIcs
        | CalendarOperation::ImportIcs
        | CalendarOperation::FinalizeLaunchImport => spec.retry_policy(RetryPolicy::AtMostOnce),
        CalendarOperation::PrepareInvitationInMail { .. } => spec
            .retry_policy(RetryPolicy::Idempotent)
            .requiring(PermissionRequirement::Invite)
            .returns(ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "object",
                    "required": ["delivery", "target", "arguments"],
                    "properties": {
                        "delivery": {"const": "not_sent"},
                        "target": {"type": "object"},
                        "arguments": {"type": "object"}
                    }
                }),
            }),
    }
}

pub(crate) fn calendar_collection_resource(calendar: &CalendarCollectionResource) -> Resource {
    let open = calendar_operation(
        "open",
        "Open calendar",
        OperationKind::View,
        &CalendarOperation::OpenCalendar {
            id: calendar.id.clone(),
        },
    );
    Resource::new(
        calendar_resource_id(&calendar.id),
        "calendar.collection",
        calendar.label.clone(),
    )
    .with_action(open)
    .with_action(calendar_operation(
        "set-visibility",
        if calendar.visible {
            "Hide calendar"
        } else {
            "Show calendar"
        },
        OperationKind::Effect,
        &CalendarOperation::SetCalendarVisible {
            id: calendar.id.clone(),
            visible: !calendar.visible,
        },
    ))
}

/// Minimal, exact event resource used by the retained global-search provider.
/// It deliberately materialises only the action named by a search result, so a
/// late search hit never has to construct the root resource tree (or an
/// event's attendees, notes, reminders and recurrence children) just to open
/// its stable address.
pub(crate) fn search_event_resource(event: &Event) -> Resource {
    Resource::new(
        event_resource_id(event.id),
        "calendar.event",
        event.display_title(),
    )
    .with_action(calendar_operation(
        "open",
        "Open event",
        OperationKind::View,
        &CalendarOperation::OpenEvent {
            id: event.id.to_string(),
        },
    ))
}

/// Minimal, exact task resource used by the retained global-search provider.
pub(crate) fn search_task_resource(task: &Task) -> Resource {
    Resource::new(
        task_resource_id(task.id),
        "calendar.task",
        if task.title.trim().is_empty() {
            "Untitled task".to_string()
        } else {
            task.title.clone()
        },
    )
    .with_action(calendar_operation(
        "open",
        "Open task",
        OperationKind::View,
        &CalendarOperation::OpenTask {
            id: task.id.to_string(),
        },
    ))
}

/// Minimal, exact project resource used by the retained global-search provider.
pub(crate) fn search_project_resource(project: &Project) -> Resource {
    Resource::new(
        project_resource_id(project.id),
        "calendar.project",
        project.name.clone(),
    )
    .with_action(calendar_operation(
        "open",
        "Open project tasks",
        OperationKind::View,
        &CalendarOperation::OpenProject {
            id: project.id.to_string(),
        },
    ))
}

/// Encode an app-local operation without allowing a serialization fault to
/// crash a desktop application. The enum only contains strings and unit
/// variants today, but the fallback keeps its failure mode explicit if that
/// changes in the future.
fn encode_operation(operation: &CalendarOperation) -> EncodedAction {
    EncodedAction::encode_operation(CALENDAR_OPERATION_TYPE, operation).unwrap_or_else(|_| {
        EncodedAction {
            type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
            payload: b"Calendar operation could not be encoded".to_vec(),
        }
    })
}

/// The datetime format events are stored in — matches [`crate::doc`].
const DT_FMT: &str = "%Y-%m-%dT%H:%M";
/// The date format tasks store their due date in.
const DATE_FMT: &str = "%Y-%m-%d";

/// A serializable snapshot of an event's scalar fields, in exactly the
/// representation the document stores (datetimes as `DT_FMT` strings, colour as
/// a palette index, recurrence as an RRULE body, an absent reminder as `-1`).
/// Carrying the document shape keeps [`Action::apply`] a pure write with no
/// re-derivation, and makes [`Self::from_event`]/[`Self::to_event`] lossless
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

fn normalize_timezone(value: &str) -> Option<String> {
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

fn email_address(value: &str) -> Option<&str> {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Apply UUID-addressed rich-text changes to an event or task note. This
    /// is the only Calendar path allowed to mutate note blocks.
    ApplyNoteOps {
        owner: NoteOwner,
        ops: Vec<BlockEditOp>,
    },
    /// A single deterministic import batch. The external file is parsed before
    /// this action is authored; replay only writes the exact payload captured
    /// here, and has one history/undo boundary.
    ImportIcs {
        events: Vec<EventData>,
        tasks: Vec<TaskData>,
    },
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
        tilo_actions::ActionContext::for_action("org.tilo.Calendar", "Calendar", action);
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
        CalendarAction::ApplyNoteOps { owner, .. } => match owner {
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
        CalDoc::update_event(doc, &event);
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
    for event in events {
        CalDoc::update_event(doc, &event);
        if let Some(notes) = CalDoc::event_notes(doc, event.id) {
            CalDoc::seed_notes_if_empty(doc, &notes, &["Imported from .ics"]);
        }
    }
    for task in tasks {
        CalDoc::update_task(doc, &task);
    }
    Ok(())
}

fn apply_note_ops(
    doc: &mut Doc,
    owner: &NoteOwner,
    ops: &[BlockEditOp],
) -> Result<(), ActionError> {
    let (event, id) = match owner {
        NoteOwner::Event { id } => (true, id),
        NoteOwner::Task { id } => (false, id),
    };
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("note owner id is invalid"))?;
    CalDoc::apply_note_ops(doc, event, id, ops).map_err(|message| refused(&message))
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
            CalendarAction::ApplyNoteOps { owner, ops } => apply_note_ops(doc, owner, ops)?,
            CalendarAction::ImportIcs { events, tasks } => apply_import_ics(doc, events, tasks)?,
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
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
// Resource tree
// ===========================================================================

/// One event surfaced as a resource, with its delete pre-resolved so the
/// palette's "Delete event" builds the byte-identical action the trash button
/// does (a synced event still queues its `CalDAV` delete).
#[derive(Debug, Clone)]
pub struct EventResource {
    pub data: EventData,
    pub remote_delete: Option<String>,
}

/// One task surfaced as a resource. Its delete fanout is bounded; when the
/// parent has more children, the delete action explicitly remains unavailable
/// until children are handled through their own stable resources.
#[derive(Debug, Clone)]
pub struct TaskResource {
    pub data: TaskData,
    pub subtasks: Vec<String>,
    /// Children omitted from `subtasks` to keep exact resource/action
    /// materialisation bounded.
    pub subtask_overflow: usize,
}

/// One durable task project plus whether deleting it would strand linked
/// calendar entities. This pre-resolved state lets the resource advertise the
/// same safe policy agents and the action executor enforce.
#[derive(Debug, Clone)]
pub struct ProjectResource {
    pub data: ProjectData,
    pub in_use: bool,
}

/// A local or remote event collection that owns event resources. Calendar
/// configuration remains in Settings, but Calendar publishes the collection's
/// stable identity, visibility and view action so navigation/search never use
/// a display label as an authority token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarCollectionResource {
    pub id: String,
    pub label: String,
    pub color: Option<usize>,
    pub visible: bool,
}

/// The presentation context the calendar hands [`provide_resources`]: a template
/// for a palette-created event (built from the current view) plus the events and
/// tasks currently in the model. Everything here is derived from app
/// state the document already holds; nothing is fabricated.
#[derive(Debug, Clone)]
pub struct CalendarResourceCtx {
    /// The event a palette "New event" starts from — its start/end/colour come
    /// from the current view; its id and title are filled at commit time.
    pub new_event: EventData,
    pub events: Vec<EventResource>,
    pub tasks: Vec<TaskResource>,
    pub projects: Vec<ProjectResource>,
    pub calendars: Vec<CalendarCollectionResource>,
    pub can_sync: bool,
    pub syncing: bool,
    pub can_export: bool,
    /// Captured from the model's display-order tail so publishing the root
    /// never needs to scan dynamic project children.
    pub next_project_order: u32,
}

impl CalendarResourceCtx {
    pub(crate) fn new(
        new_event: EventData,
        can_sync: bool,
        syncing: bool,
        can_export: bool,
        next_project_order: u32,
    ) -> Self {
        Self {
            new_event,
            events: Vec::new(),
            tasks: Vec::new(),
            projects: Vec::new(),
            calendars: Vec::new(),
            can_sync,
            syncing,
            can_export,
            next_project_order,
        }
    }

    fn empty_scoped(&self) -> Self {
        Self {
            new_event: self.new_event.clone(),
            events: Vec::new(),
            tasks: Vec::new(),
            projects: Vec::new(),
            calendars: self.calendars.clone(),
            can_sync: self.can_sync,
            syncing: self.syncing,
            can_export: self.can_export,
            next_project_order: self.next_project_order,
        }
    }
    fn event(&self, id: &str) -> Option<&EventResource> {
        self.events.iter().find(|event| event.data.id == id)
    }
    fn task(&self, id: &str) -> Option<&TaskResource> {
        self.tasks.iter().find(|task| task.data.id == id)
    }
    fn project(&self, id: &str) -> Option<&ProjectResource> {
        self.projects.iter().find(|project| project.data.id == id)
    }
    fn calendar(&self, id: &str) -> Option<&CalendarCollectionResource> {
        self.calendars.iter().find(|calendar| calendar.id == id)
    }
}

/// Whether an event may be exposed to the global resource/action surface.
///
/// Calendar visibility is a Settings-owned policy, so it is deliberately
/// evaluated from the current context rather than persisted into a replicated
/// event. Unknown owners remain available: an imported local record can
/// outlive its account configuration. A known hidden collection, however,
/// suppresses every descendant resource so its title, attendees, reminder and
/// notes cannot be discovered by search, agents, or action history.
fn event_visible_to_resource_surface(ctx: &CalendarResourceCtx, event: &EventResource) -> bool {
    let owner = if event.data.calendar_id.is_empty() {
        "local"
    } else {
        event.data.calendar_id.as_str()
    };
    ctx.calendar(owner).is_none_or(|calendar| calendar.visible)
}

/// Retain exactly the event's owning collection when materialising a nested
/// resource. Dropping it would make `provide_resources` correctly refuse to
/// attach a remote event to an unrelated calendar, but would also make a valid
/// reminder/attendee/occurrence address disappear.
fn scope_event_owner(
    ctx: &CalendarResourceCtx,
    scoped: &mut CalendarResourceCtx,
    event: &EventResource,
) {
    let owner = if event.data.calendar_id.is_empty() {
        "local"
    } else {
        event.data.calendar_id.as_str()
    };
    scoped.calendars = ctx
        .calendar(owner)
        .cloned()
        .or_else(|| {
            Some(CalendarCollectionResource {
                id: owner.to_string(),
                label: owner.to_string(),
                color: None,
                visible: true,
            })
        })
        .into_iter()
        .collect();
}

/// Return an explicit invalid-arguments envelope rather than silently turning
/// a malformed agent request into a no-op document action.
fn invalid_arguments(reason: impl Into<String>) -> EncodedAction {
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
        payload: reason.into().into_bytes(),
    }
}

/// Build a replayable update to a snapshot of one event. The resource already
/// names the exact event; the typed parameter declaration makes an agent's
/// requested field edit strict before it becomes a CRDT action.
fn event_update_spec<F>(
    id: &str,
    label: &str,
    placeholder: &str,
    params: Vec<ActionParam>,
    data: EventData,
    update: F,
) -> ActionSpec
where
    F: Fn(&EventData, &ActionArgs) -> Result<CalendarAction, String> + Send + Sync + 'static,
{
    ActionSpec::typed(
        id,
        label,
        placeholder,
        ExecutionLocus::ClientLocal,
        params,
        move |args| match update(&data, args) {
            Ok(action) => encode(&action),
            Err(reason) => invalid_arguments(reason),
        },
    )
}

/// The task counterpart to [`event_update_spec`]. Both routes build a sparse
/// action envelope from the live resource snapshot, which keeps Ctrl+K, agents
/// and collaborative replay on the same validation path.
fn task_update_spec<F>(
    id: &str,
    label: &str,
    placeholder: &str,
    params: Vec<ActionParam>,
    data: TaskData,
    update: F,
) -> ActionSpec
where
    F: Fn(&TaskData, &ActionArgs) -> Result<CalendarAction, String> + Send + Sync + 'static,
{
    ActionSpec::typed(
        id,
        label,
        placeholder,
        ExecutionLocus::ClientLocal,
        params,
        move |args| match update(&data, args) {
            Ok(action) => encode(&action),
            Err(reason) => invalid_arguments(reason),
        },
    )
}

fn recurrence_from_choice(value: &str) -> Option<Recurrence> {
    match value {
        "none" => Some(Recurrence::None),
        "daily" => Some(Recurrence::Daily),
        "weekdays" => Some(Recurrence::Weekdays),
        "weekly" => Some(Recurrence::Weekly),
        "monthly" => Some(Recurrence::Monthly),
        "yearly" => Some(Recurrence::Yearly),
        _ => None,
    }
}

fn palette_from_choice(value: &str) -> Option<Palette> {
    match value {
        "indigo" => Some(Palette::Indigo),
        "emerald" => Some(Palette::Emerald),
        "amber" => Some(Palette::Amber),
        "red" => Some(Palette::Red),
        "blue" => Some(Palette::Blue),
        "violet" => Some(Palette::Violet),
        "slate" => Some(Palette::Slate),
        _ => None,
    }
}

fn parse_local_datetime(value: &str, field: &str) -> Result<NaiveDateTime, String> {
    NaiveDateTime::parse_from_str(value, DT_FMT)
        .map_err(|_| format!("{field} must use local date-time format YYYY-MM-DDTHH:MM"))
}

/// A one-line label for an event in the palette.
fn event_label(data: &EventData) -> String {
    let t = data.title.trim();
    if t.is_empty() {
        crate::model::UNTITLED_EVENT.to_string()
    } else {
        t.to_string()
    }
}

/// A one-line label for a task, marking completion.
fn task_label(data: &TaskData) -> String {
    let t = data.title.trim();
    let base = if t.is_empty() { "Untitled task" } else { t };
    if data.done {
        format!("✓ {base}")
    } else {
        base.to_string()
    }
}

fn immediate_spec(id: &str, label: &str, action: &CalendarAction) -> ActionSpec {
    ActionSpec::immediate(id, label, CALENDAR_ACTION_TYPE, action)
        // `ActionSpec`'s fields are private now that it carries typed
        // parameters; build the fallback through the constructor that
        // takes an already-encoded action.
        .unwrap_or_else(|_| {
            ActionSpec::immediate_encoded(id, label.to_string(), action.locus(), encode(action))
        })
}

fn note_resource(parent_resource: &str, owner: NoteOwner) -> Resource {
    Resource::new(format!("{parent_resource}:notes"), "calendar.note", "Notes").with_action(
        ActionSpec::typed(
            "apply-ops",
            "Apply note operations",
            "JSON array of UUID-addressed block operations",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "ops",
                ParamType::Text,
                "Semantic note operations",
            )],
            move |args| {
                let payload = args.text("ops").unwrap_or_default();
                match serde_json::from_str::<Vec<BlockEditOp>>(payload) {
                    Ok(ops) if !ops.is_empty() => encode(&CalendarAction::ApplyNoteOps {
                        owner: owner.clone(),
                        ops,
                    }),
                    Ok(_) => invalid_arguments("at least one note operation is required"),
                    Err(_) => invalid_arguments("ops must be a JSON array of note operations"),
                }
            },
        ),
    )
}

/// Build the resource tree the calendar currently has open: the calendar itself
/// (with a "New event" action), each event (open + rename + delete), and each
/// task (open + toggle + delete). Every document-mutating operation is exported as the
/// same action the on-screen affordance dispatches.
#[allow(
    clippy::too_many_lines,
    reason = "resource publication intentionally keeps every event and task action beside its typed validator"
)]
/// Stable calendar root without dynamic events/tasks/projects.
pub fn calendar_root(ctx: &CalendarResourceCtx) -> Resource {
    let mut root = Resource::new("calendar.calendar", "calendar.calendar", "Calendar");

    root = root.with_action(
        ActionSpec::typed_operation(
            "show-view",
            "Show calendar view",
            "month / week / day / agenda",
            OperationKind::View,
            CALENDAR_OPERATION_TYPE,
            vec![
                ActionParam::required("view", ParamType::Text, "Calendar view to show")
                    .with_choices(["month", "week", "day", "agenda"]),
            ],
            |args| {
                let operation = match args.text("view").unwrap_or("month") {
                    "week" => CalendarOperation::ShowWeek,
                    "day" => CalendarOperation::ShowDay,
                    "agenda" => CalendarOperation::ShowAgenda,
                    _ => CalendarOperation::ShowMonth,
                };
                encode_operation(&operation)
            },
        )
        .retry_policy(RetryPolicy::Idempotent),
    );
    root = root.with_action(calendar_operation(
        "show-all-calendars",
        "Show all calendars",
        OperationKind::View,
        &CalendarOperation::ShowAllCalendars,
    ));
    let mut sync = calendar_operation(
        "sync",
        "Sync calendars",
        OperationKind::Effect,
        &CalendarOperation::Sync,
    );
    if ctx.syncing {
        sync = sync.unavailable(
            "already_running",
            "Calendar synchronization is already running",
        );
    } else if !ctx.can_sync {
        sync = sync.unavailable("no_accounts", "Add a calendar account before synchronizing");
    }
    root = root.with_action(sync);
    let mut export = calendar_operation(
        "export",
        "Export as ICS",
        OperationKind::Effect,
        &CalendarOperation::ExportIcs,
    );
    if !ctx.can_export {
        export = export.unavailable("calendar_empty", "There are no events or tasks to export");
    }
    root = root.with_action(export);
    // Discovery stays pure: the transfer directory is read only if this
    // explicit effect is invoked, never while the resource tree is searched or
    // refreshed. The handler turns the file into one replayable import action.
    root = root.with_action(calendar_operation(
        "import-latest-ics",
        "Import latest ICS",
        OperationKind::Effect,
        &CalendarOperation::ImportIcs,
    ));

    // Calendar-level: create an event from the current view. The uuid is minted
    // once when the title argument is committed, then baked into the envelope, so
    // replay stays deterministic.
    let template = ctx.new_event.clone();
    root = root.with_action(ActionSpec::prompted(
        "new-event",
        "New event",
        "Title",
        "event title…",
        ExecutionLocus::ClientLocal,
        move |title: &str| {
            let mut data = template.clone();
            data.id = Uuid::new_v4().to_string();
            data.title = title.trim().to_string();
            encode(&CalendarAction::UpsertEvent(data))
        },
    ));
    // Tasks share the Calendar CRDT and its history/undo model. Exposing task
    // creation here closes the former agent-only dead end where tasks could be
    // searched, toggled, and deleted but never created from a resource action.
    root = root.with_action(ActionSpec::prompted(
        "new-task",
        "New task",
        "Title",
        "task title…",
        ExecutionLocus::ClientLocal,
        |title: &str| {
            let title = title.trim();
            if title.is_empty() {
                return invalid_arguments("task title must contain non-whitespace text");
            }
            encode(&CalendarAction::UpsertTask(TaskData {
                id: Uuid::new_v4().to_string(),
                title: title.to_string(),
                done: false,
                due: String::new(),
                priority: Priority::None.rank(),
                project: String::new(),
                parent: String::new(),
            }))
        },
    ));
    let next_project_order = ctx.next_project_order;
    root = root.with_action(ActionSpec::prompted(
        "new-project",
        "New task project",
        "Name",
        "project name…",
        ExecutionLocus::ClientLocal,
        move |name: &str| {
            let name = name.trim();
            if name.is_empty() {
                return invalid_arguments("project name must contain non-whitespace text");
            }
            encode(&CalendarAction::UpsertProject(ProjectData {
                id: Uuid::new_v4().to_string(),
                name: name.to_string(),
                color: Palette::Indigo.index(),
                order: next_project_order,
            }))
        },
    ));

    root
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded eager resource projection keeps every entity's canonical ownership and action construction in one auditable routing table"
)]
pub fn provide_resources(ctx: &CalendarResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut root = calendar_root(ctx);
    // Reserve one bounded node per visible collection context (plus an
    // unfiled-task bucket) before materialising leaves. This keeps the entire
    // eager tree bounded even when one account has an enormous history; every
    // omitted stable id remains available through `resource()` below.
    let eager_calendar_count = ctx
        .calendars
        .len()
        .min(EAGER_RESOURCE_SNAPSHOT_LIMIT.saturating_sub(1));
    let mut eager_remaining = EAGER_RESOURCE_SNAPSHOT_LIMIT
        .saturating_sub(eager_calendar_count)
        .saturating_sub(1);
    for project in &ctx.projects {
        if eager_remaining == 0 {
            break;
        }
        eager_remaining = eager_remaining.saturating_sub(1);
        let id = project.data.id.clone();
        let Ok(parsed_id) = id.parse::<Id>() else {
            continue;
        };
        let mut child = Resource::new(
            project_resource_id(parsed_id),
            "calendar.project",
            project.data.name.clone(),
        )
        .with_action(calendar_operation(
            "open",
            "Open project tasks",
            OperationKind::View,
            &CalendarOperation::OpenProject { id: id.clone() },
        ));
        child = child.with_action(ActionSpec::typed(
            "edit",
            "Rename project",
            "project name…",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("name", ParamType::Text, "Project name")
                    .with_schema(serde_json::json!({"maxLength": 120})),
            ],
            move |args| {
                let name = args.text("name").unwrap_or_default().trim();
                if name.is_empty() {
                    return invalid_arguments("project name must contain non-whitespace text");
                }
                encode(&CalendarAction::PatchProject {
                    id: id.clone(),
                    patch: ProjectPatch {
                        name: Some(name.to_string()),
                        ..ProjectPatch::default()
                    },
                })
            },
        ));
        let id_for_color = project.data.id.clone();
        child = child.with_action(ActionSpec::typed(
            "set-color",
            "Set project color",
            "indigo / emerald / amber / red / blue / violet / slate",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("color", ParamType::Text, "Project colour").with_choices([
                    "indigo", "emerald", "amber", "red", "blue", "violet", "slate",
                ]),
            ],
            move |args| match palette_from_choice(args.text("color").unwrap_or_default()) {
                Some(color) => encode(&CalendarAction::PatchProject {
                    id: id_for_color.clone(),
                    patch: ProjectPatch {
                        color: Some(color.index()),
                        ..ProjectPatch::default()
                    },
                }),
                None => invalid_arguments("project color is not supported"),
            },
        ));
        let mut delete = immediate_spec(
            "delete",
            "Delete project",
            &CalendarAction::DeleteProject {
                id: project.data.id.clone(),
            },
        );
        if project.in_use {
            delete = delete.unavailable(
                "project_in_use",
                "Move or unfile linked events and tasks before deleting this project",
            );
        }
        child = child.with_action(delete);
        root = root.with_child(child);
    }

    for ev in ctx
        .events
        .iter()
        .filter(|event| event_visible_to_resource_surface(ctx, event))
    {
        if eager_remaining == 0 {
            break;
        }
        eager_remaining = eager_remaining.saturating_sub(1);
        let id = ev.data.id.clone();
        let Ok(parsed_id) = id.parse::<Id>() else {
            // A corrupted snapshot must not make resource publication crash.
            // The CRDT loader normally prevents this, but omitting one bad
            // child keeps all other calendar resources usable.
            continue;
        };
        let mut child = Resource::new(
            event_resource_id(parsed_id),
            "calendar.event",
            event_label(&ev.data),
        );
        child = child.with_action(calendar_operation(
            "open",
            "Open event",
            OperationKind::View,
            &CalendarOperation::OpenEvent { id: id.clone() },
        ));
        child = child.with_action(event_update_spec(
            "edit",
            "Rename event",
            "event title…",
            vec![
                ActionParam::required("title", ParamType::Text, "Event title")
                    .with_schema(serde_json::json!({"maxLength": 240})),
            ],
            ev.data.clone(),
            |data, args| {
                let title = args.text("title").unwrap_or_default().trim();
                if title.is_empty() {
                    return Err("title must contain non-whitespace text".into());
                }
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        title: Some(title.to_string()),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "set-location",
            "Set location",
            "location, room, or meeting link…",
            vec![
                ActionParam::required(
                    "location",
                    ParamType::Text,
                    "Event location, room, or meeting link",
                )
                .with_schema(serde_json::json!({"maxLength": 320})),
            ],
            ev.data.clone(),
            |data, args| {
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        location: Some(
                            args.text("location").unwrap_or_default().trim().to_string(),
                        ),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "set-start",
            "Set start time",
            "YYYY-MM-DDTHH:MM",
            vec![
                ActionParam::required(
                    "start",
                    ParamType::Text,
                    "Local start date and time in YYYY-MM-DDTHH:MM",
                )
                .with_schema(
                    serde_json::json!({"pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}$"}),
                ),
            ],
            ev.data.clone(),
            |data, args| {
                let start = args.text("start").unwrap_or_default();
                let start_value = parse_local_datetime(start, "start")?;
                let end_value = parse_local_datetime(&data.end, "current end")?;
                if start_value > end_value {
                    return Err("start must not be after end".into());
                }
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        start: Some(start.to_string()),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "set-end",
            "Set end time",
            "YYYY-MM-DDTHH:MM",
            vec![
                ActionParam::required(
                    "end",
                    ParamType::Text,
                    "Local end date and time in YYYY-MM-DDTHH:MM",
                )
                .with_schema(
                    serde_json::json!({"pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}$"}),
                ),
            ],
            ev.data.clone(),
            |data, args| {
                let end = args.text("end").unwrap_or_default();
                let end_value = parse_local_datetime(end, "end")?;
                let start_value = parse_local_datetime(&data.start, "current start")?;
                if end_value < start_value {
                    return Err("end must not be before start".into());
                }
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        end: Some(end.to_string()),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "set-all-day",
            "Set all-day",
            "true or false",
            vec![ActionParam::required(
                "all_day",
                ParamType::Boolean,
                "Whether this event occupies the whole day",
            )],
            ev.data.clone(),
            |data, args| {
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        all_day: Some(args.boolean("all_day").unwrap_or(false)),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "set-repeat",
            "Set repeat",
            "none / daily / weekdays / weekly / monthly / yearly",
            vec![
                ActionParam::required("repeat", ParamType::Text, "How often the event repeats")
                    .with_choices(["none", "daily", "weekdays", "weekly", "monthly", "yearly"]),
            ],
            ev.data.clone(),
            |data, args| {
                let repeat = recurrence_from_choice(args.text("repeat").unwrap_or_default())
                    .ok_or_else(|| "repeat is not supported".to_string())?;
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        rrule: Some(repeat.to_rrule()),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "set-repeat-interval",
            "Set repeat interval",
            "positive interval",
            vec![
                ActionParam::required("interval", ParamType::Integer, "Repeat every N periods")
                    .with_schema(serde_json::json!({"minimum": 1, "maximum": 999})),
            ],
            ev.data.clone(),
            |data, args| {
                let interval = u32::try_from(args.integer("interval").unwrap_or(0))
                    .map_err(|_| "interval must be a positive integer")?;
                if !(1..=999).contains(&interval) {
                    return Err("interval must be between 1 and 999".into());
                }
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        recurrence_interval: Some(interval),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "set-repeat-until",
            "Set repeat end date",
            "YYYY-MM-DD, or empty",
            vec![ActionParam::required(
                "until",
                ParamType::Text,
                "Inclusive recurrence end date, or empty to clear",
            )],
            ev.data.clone(),
            |data, args| {
                let until = args.text("until").unwrap_or_default().trim();
                if !until.is_empty() && NaiveDate::parse_from_str(until, DATE_FMT).is_err() {
                    return Err("until must be YYYY-MM-DD or empty".into());
                }
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        recurrence_until: Some(until.to_string()),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "set-repeat-count",
            "Set occurrence count",
            "0 clears the limit",
            vec![
                ActionParam::required("count", ParamType::Integer, "Maximum occurrences")
                    .with_schema(serde_json::json!({"minimum": 0, "maximum": 100_000})),
            ],
            ev.data.clone(),
            |data, args| {
                let count = u32::try_from(args.integer("count").unwrap_or(-1))
                    .map_err(|_| "count must be zero or positive")?;
                if count > 100_000 {
                    return Err("count must not exceed 100000".into());
                }
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        recurrence_count: Some(count),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "set-timezone",
            "Set time zone",
            "IANA time zone or floating",
            vec![ActionParam::required(
                "timezone",
                ParamType::Text,
                "IANA time-zone id such as Europe/Berlin, UTC, or floating",
            )],
            ev.data.clone(),
            |data, args| {
                let timezone = normalize_timezone(args.text("timezone").unwrap_or_default())
                    .ok_or_else(|| "timezone is invalid".to_string())?;
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        timezone: Some(timezone),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        let event_id_for_invite = id.clone();
        child = child.with_action(ActionSpec::typed(
            "invite",
            "Invite attendee",
            "name and email",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::optional("name", ParamType::Text, "Display name"),
                ActionParam::required("email", ParamType::Text, "Email address"),
            ],
            move |args| {
                let email = args.text("email").unwrap_or_default().trim();
                if email_address(email).is_none() {
                    return invalid_arguments("attendee email is invalid");
                }
                encode(&CalendarAction::UpsertAttendee {
                    event_id: event_id_for_invite.clone(),
                    attendee: AttendeeData {
                        id: Uuid::new_v4().to_string(),
                        name: args.text("name").unwrap_or_default().trim().to_string(),
                        email: email.to_string(),
                        status: "NEEDS-ACTION".into(),
                    },
                })
            },
        ));
        let event_id_for_exception = id.clone();
        child = child.with_action(ActionSpec::typed(
            "set-exception",
            "Include or exclude occurrence",
            "original date and excluded state",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("date", ParamType::Text, "Original date (YYYY-MM-DD)"),
                ActionParam::required("excluded", ParamType::Boolean, "Whether to skip it"),
            ],
            move |args| {
                let date = args.text("date").unwrap_or_default();
                if NaiveDate::parse_from_str(date, DATE_FMT).is_err() {
                    return invalid_arguments("date must be YYYY-MM-DD");
                }
                encode(&CalendarAction::SetRecurrenceException {
                    event_id: event_id_for_exception.clone(),
                    date: date.to_string(),
                    excluded: args.boolean("excluded").unwrap_or(false),
                })
            },
        ));
        child = child.with_action(event_update_spec(
            "set-reminder",
            "Set reminder",
            "none / 10 / 30 / 60",
            vec![
                ActionParam::required(
                    "minutes_before",
                    ParamType::Text,
                    "Minutes before the event to notify, or none",
                )
                .with_choices(["none", "10", "30", "60"]),
            ],
            ev.data.clone(),
            |data, args| {
                let reminder = match args.text("minutes_before").unwrap_or_default() {
                    "none" => -1,
                    "10" => 10,
                    "30" => 30,
                    "60" => 60,
                    _ => return Err("reminder is not supported".into()),
                };
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        reminder: Some(reminder),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "set-color",
            "Set color",
            "indigo / emerald / amber / red / blue / violet / slate",
            vec![
                ActionParam::required("color", ParamType::Text, "Event colour").with_choices([
                    "indigo", "emerald", "amber", "red", "blue", "violet", "slate",
                ]),
            ],
            ev.data.clone(),
            |data, args| {
                let color = palette_from_choice(args.text("color").unwrap_or_default())
                    .ok_or_else(|| "color is not supported".to_string())?;
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        color: Some(color.index()),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "move-to-project",
            "Move to project",
            "project destination",
            vec![ActionParam::optional(
                "project",
                ParamType::Resource,
                "calendar.project resource address; omit to unfile",
            )],
            ev.data.clone(),
            move |data, args| {
                let project = match args.text("project") {
                    None => String::new(),
                    Some(resource) => project_id_from_resource(resource)
                        .ok_or_else(|| "project must be a calendar.project resource".to_string())?
                        .to_string(),
                };
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        project: Some(project),
                        ..EventPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(event_update_spec(
            "move-to-calendar",
            "Move to calendar",
            "calendar destination",
            vec![ActionParam::required(
                "calendar",
                ParamType::Resource,
                "calendar.collection resource address (calendar.collection:local is on-device)",
            )],
            ev.data.clone(),
            move |data, args| {
                let target = args
                    .text("calendar")
                    .and_then(calendar_id_from_resource)
                    .ok_or_else(|| "calendar must be a calendar.collection resource".to_string())?;
                if target == "local" {
                    Ok(patch_event_action(
                        data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                        EventPatch {
                            calendar_id: Some(String::new()),
                            enqueue_remote: Some(false),
                            ..EventPatch::default()
                        },
                    ))
                } else {
                    Ok(patch_event_action(
                        data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                        EventPatch {
                            calendar_id: Some(target.to_string()),
                            enqueue_remote: Some(true),
                            ..EventPatch::default()
                        },
                    ))
                }
            },
        ));
        child = child.with_action(immediate_spec(
            "delete",
            "Delete event",
            &delete_event_action(id.clone(), ev.remote_delete.clone()),
        ));
        child = child.with_child(note_resource(
            &event_resource_id(parsed_id),
            NoteOwner::Event { id: id.clone() },
        ));
        if ev.data.reminder >= 0 {
            let event_id_s = ev.data.id.clone();
            let event_id = parsed_id;
            let reminder = Resource::new(
                reminder_resource_id(event_id),
                "calendar.reminder",
                format!("{} minutes before", ev.data.reminder),
            )
            .with_action(event_update_spec(
                "set-minutes-before",
                "Set reminder time",
                "10 / 30 / 60",
                vec![
                    ActionParam::required(
                        "minutes_before",
                        ParamType::Integer,
                        "Minutes before the event",
                    )
                    .with_schema(serde_json::json!({"enum": [10, 30, 60]})),
                ],
                ev.data.clone(),
                |data, args| {
                    let value = args.integer("minutes_before").unwrap_or(-1);
                    if !matches!(value, 10 | 30 | 60) {
                        return Err("reminder must be 10, 30, or 60 minutes".into());
                    }
                    Ok(CalendarAction::PatchEvent {
                        id: data.id.clone(),
                        patch: EventPatch {
                            reminder: Some(value),
                            ..EventPatch::default()
                        },
                    })
                },
            ))
            .with_action(immediate_spec(
                "remove",
                "Remove reminder",
                &CalendarAction::PatchEvent {
                    id: event_id_s,
                    patch: EventPatch {
                        reminder: Some(-1),
                        ..EventPatch::default()
                    },
                },
            ));
            child = child.with_child(reminder);
        }
        for attendee in ev.data.attendees.iter().take(24) {
            let Ok(event_id) = ev.data.id.parse::<Id>() else {
                continue;
            };
            let Ok(attendee_id) = attendee.id.parse::<Id>() else {
                continue;
            };
            let event_id_s = ev.data.id.clone();
            let attendee_id_s = attendee.id.clone();
            let mut attendee_resource = Resource::new(
                attendee_resource_id(event_id, attendee_id),
                "calendar.attendee",
                if attendee.name.is_empty() {
                    attendee.email.clone()
                } else {
                    attendee.name.clone()
                },
            );
            let patch_event_id = event_id_s.clone();
            let patch_attendee_id = attendee_id_s.clone();
            attendee_resource =
                attendee_resource.with_action(ActionSpec::typed(
                    "respond",
                    "Set invitation response",
                    "accepted / tentative / declined / needs-action",
                    ExecutionLocus::ClientLocal,
                    vec![
                        ActionParam::required("status", ParamType::Text, "Response")
                            .with_choices(["ACCEPTED", "TENTATIVE", "DECLINED", "NEEDS-ACTION"]),
                    ],
                    move |args| {
                        encode(&CalendarAction::PatchAttendee {
                            event_id: patch_event_id.clone(),
                            attendee_id: patch_attendee_id.clone(),
                            patch: AttendeePatch {
                                status: Some(args.text("status").unwrap_or("NEEDS-ACTION").into()),
                                ..AttendeePatch::default()
                            },
                        })
                    },
                ));
            attendee_resource = attendee_resource.with_action(immediate_spec(
                "make-organizer",
                "Make organizer",
                &CalendarAction::PatchEvent {
                    id: event_id_s.clone(),
                    patch: EventPatch {
                        organizer: Some(attendee_id_s.clone()),
                        ..EventPatch::default()
                    },
                },
            ));
            attendee_resource = attendee_resource.with_action(calendar_operation(
                "prepare-in-mail",
                "Prepare invitation in Mail",
                OperationKind::Effect,
                &CalendarOperation::PrepareInvitationInMail {
                    event_id: event_id_s.clone(),
                    attendee_id: attendee_id_s.clone(),
                },
            ));
            attendee_resource = attendee_resource.with_action(immediate_spec(
                "remove",
                "Remove attendee",
                &CalendarAction::DeleteAttendee {
                    event_id: event_id_s,
                    attendee_id: attendee_id_s,
                },
            ));
            child = child.with_child(attendee_resource);
        }
        let occurrence_dates = ev
            .data
            .recurrence_exdates
            .iter()
            .chain(
                ev.data
                    .occurrence_overrides
                    .iter()
                    .map(|edit| &edit.original_date),
            )
            .filter_map(|date| NaiveDate::parse_from_str(date, DATE_FMT).ok())
            .collect::<std::collections::BTreeSet<_>>();
        for original_date in occurrence_dates.into_iter().take(24) {
            let Ok(event_id) = ev.data.id.parse::<Id>() else {
                continue;
            };
            let mut occurrence = Resource::new(
                occurrence_resource_id(event_id, original_date),
                "calendar.occurrence",
                format!("{} · {original_date}", event_label(&ev.data)),
            )
            .with_action(calendar_operation(
                "open",
                "Open occurrence",
                OperationKind::View,
                &CalendarOperation::OpenOccurrence {
                    event_id: ev.data.id.clone(),
                    original_date: original_date.to_string(),
                },
            ));
            occurrence = occurrence.with_action(immediate_spec(
                "exclude",
                "Skip occurrence",
                &CalendarAction::SetRecurrenceException {
                    event_id: ev.data.id.clone(),
                    date: original_date.to_string(),
                    excluded: true,
                },
            ));
            occurrence = occurrence.with_action(immediate_spec(
                "restore",
                "Restore occurrence",
                &CalendarAction::SetRecurrenceException {
                    event_id: ev.data.id.clone(),
                    date: original_date.to_string(),
                    excluded: false,
                },
            ));
            child = child.with_child(occurrence);
        }
        root = root.with_child(child);
    }

    for tk in &ctx.tasks {
        if eager_remaining == 0 {
            break;
        }
        eager_remaining = eager_remaining.saturating_sub(1);
        let id = tk.data.id.clone();
        let Ok(parsed_id) = id.parse::<Id>() else {
            continue;
        };
        let toggle_label = if tk.data.done {
            "Mark not done"
        } else {
            "Mark done"
        };
        let mut child = Resource::new(
            task_resource_id(parsed_id),
            "calendar.task",
            task_label(&tk.data),
        )
        .with_action(calendar_operation(
            "open",
            "Open task",
            OperationKind::View,
            &CalendarOperation::OpenTask { id: id.clone() },
        ))
        .with_action(immediate_spec(
            "toggle",
            toggle_label,
            &set_task_done_action(id.clone(), !tk.data.done),
        ));
        child = child.with_action(task_update_spec(
            "edit",
            "Rename task",
            "task title…",
            vec![
                ActionParam::required("title", ParamType::Text, "Task title")
                    .with_schema(serde_json::json!({"maxLength": 240})),
            ],
            tk.data.clone(),
            |data, args| {
                let title = args.text("title").unwrap_or_default().trim();
                if title.is_empty() {
                    return Err("title must contain non-whitespace text".into());
                }
                Ok(patch_task_action(
                    data.id.parse::<Id>().map_err(|_| "task id is invalid")?,
                    TaskPatch {
                        title: Some(title.to_string()),
                        ..TaskPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(task_update_spec(
            "set-due",
            "Set due date",
            "YYYY-MM-DD or none",
            vec![
                ActionParam::required("due", ParamType::Text, "Due date, or none to clear it")
                    .with_schema(serde_json::json!({"pattern": "^(none|\\d{4}-\\d{2}-\\d{2})$"})),
            ],
            tk.data.clone(),
            |data, args| {
                let value = args.text("due").unwrap_or_default();
                let due = if value == "none" {
                    String::new()
                } else {
                    NaiveDate::parse_from_str(value, DATE_FMT)
                        .map_err(|_| "due must use YYYY-MM-DD or none")?;
                    value.to_string()
                };
                Ok(patch_task_action(
                    data.id.parse::<Id>().map_err(|_| "task id is invalid")?,
                    TaskPatch {
                        due: Some(due),
                        ..TaskPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(task_update_spec(
            "set-priority",
            "Set priority",
            "none / p1 / p2 / p3",
            vec![
                ActionParam::required("priority", ParamType::Text, "Task priority")
                    .with_choices(["none", "p1", "p2", "p3"]),
            ],
            tk.data.clone(),
            |data, args| {
                let priority = match args.text("priority").unwrap_or_default() {
                    "p1" => Priority::High,
                    "p2" => Priority::Medium,
                    "p3" => Priority::Low,
                    "none" => Priority::None,
                    _ => return Err("priority is not supported".into()),
                };
                Ok(patch_task_action(
                    data.id.parse::<Id>().map_err(|_| "task id is invalid")?,
                    TaskPatch {
                        priority: Some(priority.rank()),
                        ..TaskPatch::default()
                    },
                ))
            },
        ));
        child = child.with_action(task_update_spec(
            "move-to-project",
            "Move to project",
            "project destination",
            vec![ActionParam::optional(
                "project",
                ParamType::Resource,
                "calendar.project resource address; omit to unfile",
            )],
            tk.data.clone(),
            move |data, args| {
                let project = match args.text("project") {
                    None => String::new(),
                    Some(resource) => project_id_from_resource(resource)
                        .ok_or_else(|| "project must be a calendar.project resource".to_string())?
                        .to_string(),
                };
                Ok(patch_task_action(
                    data.id.parse::<Id>().map_err(|_| "task id is invalid")?,
                    TaskPatch {
                        project: Some(project),
                        ..TaskPatch::default()
                    },
                ))
            },
        ));
        let mut delete = immediate_spec(
            "delete",
            "Delete task",
            &delete_task_action(id.clone(), tk.subtasks.clone()),
        );
        if tk.subtask_overflow > 0 {
            delete = delete.unavailable(
                "task_children_overflow",
                format!(
                    "This task has {} additional children; handle them first before deleting the parent",
                    tk.subtask_overflow
                ),
            );
        }
        child = child.with_action(delete);
        child = child.with_child(note_resource(
            &task_resource_id(parsed_id),
            NoteOwner::Task { id: id.clone() },
        ));
        root = root.with_child(child);
    }

    // Leaves are constructed through their normal action builders above, then
    // placed under their *actual* owning collection. If an owner itself fell
    // outside the eager cap, omit the leaf rather than attaching it to a
    // misleading sibling; direct stable-id resolution remains authoritative.
    let flat_children = std::mem::take(&mut root.children);
    let mut calendars: Vec<_> = ctx
        .calendars
        .iter()
        .take(eager_calendar_count)
        .map(calendar_collection_resource)
        .collect();
    let mut projects = Vec::new();
    let mut remaining = Vec::new();
    for child in flat_children {
        if child.kind == "calendar.project" {
            projects.push(child);
        } else {
            remaining.push(child);
        }
    }
    let mut unfiled = Resource::new(
        "calendar.tasks:unfiled",
        "calendar.task-group",
        "Unfiled tasks",
    );
    for child in remaining {
        match child.kind.as_str() {
            "calendar.event" => {
                let Some(raw) = child.id.as_str().strip_prefix("calendar.event:") else {
                    continue;
                };
                let Some(event) = ctx.events.iter().find(|event| event.data.id == raw) else {
                    continue;
                };
                let owner = event.data.calendar_id.as_str();
                let owner = if owner.is_empty() { "local" } else { owner };
                if let Some(calendar) = calendars
                    .iter_mut()
                    .find(|calendar| calendar.id.as_str() == calendar_resource_id(owner))
                {
                    calendar.children.push(child);
                }
            }
            "calendar.task" => {
                let Some(raw) = child.id.as_str().strip_prefix("calendar.task:") else {
                    continue;
                };
                let Some(task) = ctx.tasks.iter().find(|task| task.data.id == raw) else {
                    continue;
                };
                if task.data.project.is_empty() {
                    unfiled.children.push(child);
                } else if let Some(project) = projects.iter_mut().find(|project| {
                    project.id.as_str() == format!("calendar.project:{}", task.data.project)
                }) {
                    project.children.push(child);
                }
            }
            _ => {}
        }
    }
    root.children = calendars;
    root.children.extend(projects);
    if !unfiled.children.is_empty() {
        root.children.push(unfiled);
    }
    vec![root]
}

fn find_resource(resources: Vec<Resource>, id: &str) -> Option<Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource);
        }
        if let Some(found) = find_resource(resource.children.clone(), id) {
            return Some(found);
        }
    }
    None
}

/// Build exactly one current resource for a stable id omitted from the eager
/// snapshot. The context is re-derived from the live model first, so an id
/// removed by a peer, import reconciliation, or undo is refused rather than
/// accidentally resolving to a similarly-positioned resource.
#[allow(
    clippy::too_many_lines,
    reason = "stable-id routing keeps every ownership/staleness branch adjacent for auditability"
)]
fn resource_from_ctx(ctx: &CalendarResourceCtx, doc: &AutoCommit, id: &str) -> Option<Resource> {
    let mut scoped = ctx.empty_scoped();

    if id == "calendar.calendar" {
        return find_resource(provide_resources(&scoped, doc), id);
    }

    if let Some(raw) = id.strip_prefix("calendar.collection:") {
        let calendar = ctx
            .calendar(raw)
            .filter(|_| calendar_resource_id(raw) == id)?;
        scoped.calendars = vec![calendar.clone()];
        // Resolve only the addressed container. Its potentially unbounded
        // event history remains independently addressable by stable id.
        return find_resource(provide_resources(&scoped, doc), id);
    }

    if let Some(raw) = id.strip_prefix("calendar.project:") {
        let parsed = raw.parse::<Id>().ok()?;
        let project = ctx
            .project(raw)
            .filter(|_| project_resource_id(parsed) == id)?;
        scoped.calendars.clear();
        scoped.projects.push(project.clone());
        // Do not materialise/scan an unbounded project task history merely to
        // resolve the project container; tasks resolve by their own ids.
        return find_resource(provide_resources(&scoped, doc), id);
    }

    if let Some(raw) = id.strip_prefix("calendar.attendee:") {
        let (event_raw, attendee_raw) = raw.split_once(':')?;
        let event_id = event_raw.parse::<Id>().ok()?;
        let attendee_id = attendee_raw.parse::<Id>().ok()?;
        if attendee_resource_id(event_id, attendee_id) != id {
            return None;
        }
        let mut event = ctx.event(event_raw)?.clone();
        if !event_visible_to_resource_surface(ctx, &event) {
            return None;
        }
        let attendee = event
            .data
            .attendees
            .iter()
            .find(|attendee| attendee.id == attendee_raw)?
            .clone();
        event.data.attendees = vec![attendee];
        event.data.recurrence_exdates.clear();
        event.data.occurrence_overrides.clear();
        scope_event_owner(ctx, &mut scoped, &event);
        scoped.events.push(event);
        return find_resource(provide_resources(&scoped, doc), id);
    }

    if let Some(raw) = id.strip_prefix("calendar.reminder:") {
        let event_raw = raw.strip_suffix(":primary")?;
        let event_id = event_raw.parse::<Id>().ok()?;
        if reminder_resource_id(event_id) != id {
            return None;
        }
        let event = ctx
            .event(event_raw)
            .filter(|event| event.data.reminder >= 0)?
            .clone();
        if !event_visible_to_resource_surface(ctx, &event) {
            return None;
        }
        scope_event_owner(ctx, &mut scoped, &event);
        scoped.events.push(event);
        return find_resource(provide_resources(&scoped, doc), id);
    }

    if let Some(raw) = id.strip_prefix("calendar.occurrence:") {
        let (event_raw, date_raw) = raw.split_once(':')?;
        let event_id = event_raw.parse::<Id>().ok()?;
        let original_date = NaiveDate::parse_from_str(date_raw, DATE_FMT).ok()?;
        if occurrence_resource_id(event_id, original_date) != id {
            return None;
        }
        let mut event = ctx.event(event_raw)?.clone();
        if !event_visible_to_resource_surface(ctx, &event) {
            return None;
        }
        let mut base = event.data.to_event()?;
        base.recurrence_exdates.clear();
        base.occurrence_overrides.clear();
        if !base.occurs_on(original_date) {
            return None;
        }
        event
            .data
            .recurrence_exdates
            .retain(|date| date == date_raw);
        event
            .data
            .occurrence_overrides
            .retain(|edit| edit.original_date == date_raw);
        if event.data.recurrence_exdates.is_empty() && event.data.occurrence_overrides.is_empty() {
            event.data.recurrence_exdates.push(date_raw.to_string());
        }
        event.data.attendees.clear();
        scope_event_owner(ctx, &mut scoped, &event);
        scoped.events.push(event);
        return find_resource(provide_resources(&scoped, doc), id);
    }

    if let Some(raw) = id.strip_prefix("calendar.event:") {
        let (raw, notes) = raw
            .strip_suffix(":notes")
            .map_or((raw, false), |raw| (raw, true));
        let parsed = raw.parse::<Id>().ok()?;
        let resource_id = event_resource_id(parsed);
        if id != resource_id && (!notes || id != format!("{resource_id}:notes")) {
            return None;
        }
        let event = ctx.event(raw)?;
        if !event_visible_to_resource_surface(ctx, event) {
            return None;
        }
        scope_event_owner(ctx, &mut scoped, event);
        scoped.events.push(event.clone());
        return find_resource(provide_resources(&scoped, doc), id);
    }

    if let Some(raw) = id.strip_prefix("calendar.task:") {
        let (raw, notes) = raw
            .strip_suffix(":notes")
            .map_or((raw, false), |raw| (raw, true));
        let parsed = raw.parse::<Id>().ok()?;
        let resource_id = task_resource_id(parsed);
        if id != resource_id && (!notes || id != format!("{resource_id}:notes")) {
            return None;
        }
        let task = ctx.task(raw)?;
        scoped.calendars.clear();
        if !task.data.project.is_empty()
            && let Some(project) = ctx.project(&task.data.project)
        {
            scoped.projects.push(project.clone());
        }
        scoped.tasks.push(task.clone());
        return find_resource(provide_resources(&scoped, doc), id);
    }

    None
}

impl ResourceProvider for crate::app::CalendarApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.calendar_resource_ctx(), doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        self.exact_resource_ctx(id)
            .and_then(|ctx| resource_from_ctx(&ctx, doc, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CalendarApp;
    use automerge::ROOT;
    use tilo_actions::{ActionInput, LocalTransport};
    use tilo_automerge::ApplyIncoming;
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
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

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } | ActionInput::Deferred { .. } => {
                panic!("expected an immediate action: {}", spec.id)
            }
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
    }

    /// A demo calendar synced into a fresh document, plus the app that owns it.
    fn seeded() -> (CalendarApp, AutoCommit) {
        let mut app = CalendarApp::new_demo();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    /// Ctrl+K on the calendar yields the tree: the calendar with a "New event"
    /// action, an `calendar.event` child per event exporting open/rename/delete,
    /// and a `calendar.task` child per task exporting open/toggle/delete.
    /// Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_calendar_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one calendar root");
        let cal = &resources[0];
        assert_eq!(cal.kind, "calendar.calendar");
        let cal_actions: Vec<&str> = cal.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(cal_actions.contains(&"new-event"), "{cal_actions:?}");
        assert!(cal_actions.contains(&"new-project"), "{cal_actions:?}");
        let calendar = find_kind(&cal.children, "calendar.collection")
            .expect("a calendar collection resource");
        assert!(calendar.actions.iter().any(|action| action.id == "open"));

        let project = find_kind(&cal.children, "calendar.project").expect("a project resource");
        let project_actions: Vec<&str> = project.actions.iter().map(|a| a.id.as_str()).collect();
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

        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("New event")), "{labels:?}");
        assert!(labels.iter().any(|l| l.contains("New task")), "{labels:?}");
        // The neutral action list is deliberately display-capped. Filtering
        // must still bring every deeper event action into the same palette.
        palette.set_query("set reminder");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Set reminder")),
            "{labels:?}"
        );
    }

    #[test]
    fn large_resource_sets_are_capped_but_late_ids_resolve_lazily_and_stale_ids_refuse() {
        let (mut app, mut doc) = seeded();
        let template = CalDoc::read_model(&doc).events[0].clone();
        let mut late_id = template.id;
        for offset in 0..(EAGER_RESOURCE_SNAPSHOT_LIMIT + 2) {
            let mut event = template.clone();
            event.id = Uuid::from_u128(10_000 + offset as u128);
            event.title = format!("lazy resource event {offset}");
            late_id = event.id;
            CalendarAction::UpsertEvent(EventData::from_event(&event))
                .apply(&mut doc)
                .expect("valid event is accepted");
        }
        app.sync(&mut doc, &ROOT);

        let root = Widget::resources(&app, &doc).remove(0);
        assert!(
            root.children.len() <= EAGER_RESOURCE_SNAPSHOT_LIMIT,
            "the open-resource snapshot must stay bounded"
        );
        let late = event_resource_id(late_id);
        let resolved = <CalendarApp as ResourceProvider>::resource(&app, &doc, &late)
            .expect("a current late event resolves on demand");
        assert_eq!(resolved.id.as_str(), late);
        assert!(resolved.actions.iter().any(|action| action.id == "open"));

        CalendarAction::DeleteEvent {
            id: late_id.to_string(),
            remote_delete: None,
        }
        .apply(&mut doc)
        .expect("delete is accepted");
        app.sync(&mut doc, &ROOT);
        assert!(
            <CalendarApp as ResourceProvider>::resource(&app, &doc, &late).is_none(),
            "a deleted stable id is refused rather than redirected"
        );
    }

    #[test]
    fn omitted_calendar_keeps_its_own_bounded_hierarchy_and_refuses_when_stale() {
        let (app, doc) = seeded();
        let mut ctx = app.calendar_resource_ctx();
        let template = ctx.calendars[0].clone();
        ctx.calendars = (0..(EAGER_RESOURCE_SNAPSHOT_LIMIT + 3))
            .map(|index| crate::actions::CalendarCollectionResource {
                id: format!("remote/{index}"),
                label: format!("Remote calendar {index}"),
                color: template.color,
                visible: true,
            })
            .collect();
        let mut event = ctx.events[0].clone();
        event.data.id = Uuid::from_u128(42_4242).to_string();
        event.data.title = "event in omitted calendar".into();
        event.data.calendar_id = format!("remote/{}", EAGER_RESOURCE_SNAPSHOT_LIMIT + 2);
        ctx.events = vec![event.clone()];
        ctx.tasks.clear();
        ctx.projects.clear();

        let resources = provide_resources(&ctx, &doc);
        assert!(
            !resources[0]
                .children
                .iter()
                .any(|resource| resource.id.as_str()
                    == calendar_resource_id(&event.data.calendar_id)),
            "the late collection is omitted from the bounded eager snapshot"
        );
        let calendar_id = calendar_resource_id(&event.data.calendar_id);
        let resolved = resource_from_ctx(&ctx, &doc, &calendar_id)
            .expect("an omitted current calendar resolves directly");
        assert_eq!(resolved.id.as_str(), calendar_id);
        assert!(
            resolved.children.is_empty(),
            "exact container lookup must not expand an unbounded event history"
        );
        let event_id = format!("calendar.event:{}", event.data.id);
        assert!(
            resource_from_ctx(&ctx, &doc, &event_id).is_some(),
            "the omitted collection's event remains independently addressable"
        );

        ctx.calendars
            .retain(|calendar| calendar.id != event.data.calendar_id);
        assert!(
            resource_from_ctx(&ctx, &doc, &calendar_id).is_none(),
            "a removed collection id is not redirected to a surviving calendar"
        );
    }

    #[test]
    fn lazy_project_context_contains_only_its_own_bounded_tasks() {
        let (app, doc) = seeded();
        let ctx = app.calendar_resource_ctx();
        let project = ctx.projects[0].clone();
        let project_id = project.data.id;
        let resource = resource_from_ctx(
            &ctx,
            &doc,
            &project_resource_id(project_id.parse().unwrap()),
        )
        .expect("current project resolves lazily");
        assert!(
            resource.children.is_empty(),
            "exact project lookup must not expand an unbounded task history"
        );
    }

    #[test]
    fn deep_resource_actions_resolve_from_the_bounded_directory_by_exact_id() {
        fn collect_deep<'a>(resource: &'a Resource, found: &mut Vec<&'a Resource>) {
            if matches!(
                resource.kind.as_str(),
                "calendar.note" | "calendar.reminder" | "calendar.attendee" | "calendar.occurrence"
            ) {
                found.push(resource);
            }
            for child in &resource.children {
                collect_deep(child, found);
            }
        }

        let (app, doc) = seeded();
        let ctx = app.calendar_resource_ctx();
        let root = provide_resources(&ctx, &doc).remove(0);
        let mut deep = Vec::new();
        collect_deep(&root, &mut deep);
        assert!(
            !deep.is_empty(),
            "the demo contains note/reminder/attendee/occurrence resources"
        );

        for resource in deep {
            let Some(resolved) = resource_from_ctx(&ctx, &doc, resource.id.as_str()) else {
                panic!(
                    "a deep canonical resource resolves without expanding the root tree: {}",
                    resource.id.as_str()
                );
            };
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

    #[test]
    fn hidden_collection_withholds_event_resources_and_all_descendants() {
        let (app, doc) = seeded();
        let mut ctx = app.calendar_resource_ctx();
        let event = &mut ctx.events[0];
        let event_id = event.data.id.parse::<Id>().unwrap();
        event.data.title = "Private planning session".into();
        event.data.calendar_id = "work/private".into();
        event.data.reminder = 30;
        let attendee = AttendeeData {
            id: Uuid::new_v4().to_string(),
            name: "Private guest".into(),
            email: "guest@example.test".into(),
            status: "NEEDS-ACTION".into(),
        };
        let attendee_id = attendee.id.parse::<Id>().unwrap();
        event.data.attendees = vec![attendee];
        ctx.calendars = vec![CalendarCollectionResource {
            id: "work/private".into(),
            label: "Private work".into(),
            color: None,
            visible: false,
        }];

        let root = provide_resources(&ctx, &doc).remove(0);
        let private_collection = root
            .children
            .iter()
            .find(|resource| resource.id.as_str() == calendar_resource_id("work/private"))
            .expect("the collection remains available to re-enable");
        assert!(private_collection.children.is_empty());

        for resource_id in [
            event_resource_id(event_id),
            format!("{}:notes", event_resource_id(event_id)),
            reminder_resource_id(event_id),
            attendee_resource_id(event_id, attendee_id),
        ] {
            assert!(
                resource_from_ctx(&ctx, &doc, &resource_id).is_none(),
                "hidden collection leaked {resource_id}"
            );
        }
    }

    /// Search activation stays entirely inside the resource/action door for
    /// both content types and invokes the exact exported view operation.
    #[test]
    fn search_rows_invoke_exported_event_and_task_view_actions() {
        let (mut app, doc) = seeded();

        let resources = Widget::resources(&app, &doc);
        let all_hits = Widget::search_providers(&app)[0].search("");
        assert!(
            all_hits.iter().all(|hit| {
                hit.action_address().is_some_and(|address| {
                    tilo_actions::door::resolve(&resources, &address).is_some()
                })
            }),
            "every event, task and subtask hit resolves in the live resource tree"
        );

        for (query, expected_label, expected_kind) in [
            ("bright smiles", "Dentist", "calendar.event"),
            ("renew passport", "Renew passport", "calendar.task"),
        ] {
            let resources = Widget::resources(&app, &doc);
            let mut palette = ui::ActionPalette::new();
            palette.load_open_resources(&resources);
            palette.set_providers(Widget::search_providers(&app));
            palette.open();
            palette.set_query(query);
            let labels = palette.result_labels();
            let row = labels
                .iter()
                .position(|label| label == expected_label)
                .expect("the searchable content row is present");
            palette.set_cursor(row);

            palette.activate_selected();
            let operation = palette
                .take_operation()
                .expect("canonical hit queued a broker operation");
            assert_eq!(operation.kind, OperationKind::View);
            let resource_id = operation
                .context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str())
                .expect("operation retains its resource identity");
            assert!(resource_id.starts_with(expected_kind), "{resource_id}");
            assert!(
                palette.take_navigation().is_none(),
                "legacy callback was not used"
            );
            assert!(
                Widget::invoke_resource_operation(
                    &mut app,
                    operation.kind,
                    &operation
                        .encoded()
                        .expect("test requires prepared encoding")
                        .type_id,
                    &operation
                        .encoded()
                        .expect("test requires prepared encoding")
                        .payload,
                    &doc,
                    &ROOT,
                )
                .expect("view operation succeeds")
            );
            let id = resource_id
                .rsplit_once(':')
                .and_then(|(_, id)| id.parse::<Id>().ok())
                .expect("resource id ends in the model UUID");
            if expected_kind == "calendar.event" {
                assert_eq!(app.selected_event(), Some(id), "exact event was selected");
            } else {
                assert_eq!(app.selected_task(), Some(id), "exact task was selected");
            }
        }

        let dentist = app
            .calendar_resource_ctx()
            .events
            .iter()
            .find(|event| event.data.title == "Dentist")
            .and_then(|event| event.data.id.parse::<Id>().ok())
            .expect("Dentist event remains in the model");
        let passport = app
            .calendar_resource_ctx()
            .tasks
            .iter()
            .find(|task| task.data.title == "Renew passport")
            .and_then(|task| task.data.id.parse::<Id>().ok())
            .expect("passport task remains in the model");
        assert_ne!(
            app.selected_event(),
            Some(dentist),
            "task activation replaced event selection"
        );
        assert_eq!(
            app.selected_task(),
            Some(passport),
            "task activation selected the exact task"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the document and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        // A real document with the demo calendar in it.
        let mut app = CalendarApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);

        let victim = CalDoc::read_model(&state.doc).events[0].id;
        let before_heads = state.doc.get_heads();
        let before = CalDoc::read_model(&state.doc).events.len();

        let action = delete_event_action(victim.to_string(), None);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            CalDoc::read_model(&state.doc).events.len(),
            before - 1,
            "the action deleted an event"
        );
        assert!(
            !CalDoc::read_model(&state.doc)
                .events
                .iter()
                .any(|e| e.id == victim),
            "the right event is gone"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            CalDoc::read_model(&state.doc).events.len(),
            before,
            "undo reverted the deletion and nothing else"
        );
        assert!(
            CalDoc::read_model(&state.doc)
                .events
                .iter()
                .any(|e| e.id == victim),
            "the event is back after undo"
        );
    }

    #[test]
    fn note_ops_replay_identically_through_the_action_executor() {
        let (_app, seeded_doc) = seeded();
        let mut author = seeded_doc;
        author.commit();
        let bytes = author.save();
        let mut peer = AutoCommit::load(&bytes).expect("same base document");
        let event = CalDoc::read_model(&author).events[0].id;
        let action = CalendarAction::ApplyNoteOps {
            owner: NoteOwner::Event {
                id: event.to_string(),
            },
            ops: vec![BlockEditOp::Insert {
                block_id: Uuid::new_v4().to_string(),
                after: None,
                kind: "paragraph".to_string(),
                text: "Résumé ✓".to_string(),
            }],
        };
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).expect("calendar note action");
        let mut transport = LocalTransport::new();
        let envelope =
            tilo_actions::dispatch(&mut author, decoded.as_ref(), encoded, &mut transport)
                .expect("note action dispatches");
        tilo_actions::replay(&mut peer, envelope.envelope(), &registry())
            .expect("note action replays");

        assert_eq!(
            author.get_heads(),
            peer.get_heads(),
            "note edits converge exactly"
        );
        assert_eq!(
            CalDoc::note_text(&author, true, event),
            "Résumé ✓",
            "the action, rather than the widget, persisted the note"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        // Completion is an absolute value, so replay does not depend on the
        // receiving replica's current task state.
        let task = CalDoc::read_model(&a)
            .tasks
            .into_iter()
            .find(|t| !t.done)
            .unwrap();
        let action = set_task_done_action(task.id.to_string(), true);
        let encoded = encode(&action);

        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert!(
            CalDoc::read_model(&a).task(task.id).unwrap().done,
            "the toggle landed on both replicas"
        );
        assert!(CalDoc::read_model(&b).task(task.id).unwrap().done);
    }

    /// Replaying an `UpsertEvent` (an edit) also converges byte-identically —
    /// the create/edit/move path, not just the toggle.
    #[test]
    fn upsert_event_replays_byte_identical() {
        let (_app, seeded_doc) = seeded();
        let mut a = seeded_doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();

        let mut ev = CalDoc::read_model(&a).events[0].clone();
        ev.title = "Renamed via action".into();
        let action = upsert_event_action(&ev);
        let encoded = encode(&action);

        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(a.get_heads(), b.get_heads(), "edit replayed identically");
        assert_eq!(
            CalDoc::read_model(&b).event(ev.id).unwrap().title,
            "Renamed via action"
        );
    }

    /// Single source of truth: the palette's "Delete event" spec and the builder
    /// the trash button calls encode byte-identically — including the resolved
    /// remote-delete of a synced event.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let cal = &resources[0];
        let event = find_kind(&cal.children, "calendar.event").expect("seeded event resource");
        let id = event.id.as_str().trim_start_matches("calendar.event:");

        let delete_spec = event.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(delete_spec);
        // The ctx resolves the same remote-delete the trash-button handler does.
        let ctx = app.calendar_resource_ctx();
        let remote = ctx
            .events
            .iter()
            .find(|e| e.data.id == id)
            .and_then(|e| e.remote_delete.clone());
        let from_builder = encode(&delete_event_action(id.to_string(), remote));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth for delete"
        );

        // And the rename spec produces a field-level patch, not a stale whole
        // event snapshot that could overwrite another collaborator's edit.
        let event_id = id.parse().unwrap();
        let edit_spec = event.actions.iter().find(|a| a.id == "edit").unwrap();
        let from_spec = prompted_encode(edit_spec, "New title");
        let from_builder = encode(&patch_event_action(
            event_id,
            EventPatch {
                title: Some("New title".into()),
                ..EventPatch::default()
            },
        ));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );
        let blank = prompted_encode(edit_spec, "   ");
        assert_eq!(
            blank.type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
            "rename refuses a whitespace-only title"
        );
    }

    #[test]
    fn import_history_points_at_the_actual_calendar_root() {
        let context = history_context(&CalendarAction::ImportIcs {
            events: Vec::new(),
            tasks: Vec::new(),
        });
        assert_eq!(
            context.resource.expect("root resource").id,
            "calendar.calendar"
        );
    }

    #[test]
    fn upserts_reject_unknown_project_relationships() {
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

    #[test]
    fn hostile_patch_is_refused_atomically_before_remote_queueing() {
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

    #[test]
    fn hostile_task_delete_cannot_name_an_unrelated_task() {
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

    /// Dispatching the "New event" prompted action creates an event in the
    /// document with the committed title.
    #[test]
    fn new_event_action_creates_an_event() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|a| a.id == "new-event")
            .unwrap();
        let before = CalDoc::read_model(&doc).events.len();

        let encoded = prompted_encode(spec, "Coffee with Dana");
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

    /// The event editor's full state is not trapped behind pointer-only
    /// popovers: agents receive strict typed controls that produce the same
    /// replayable field patches as direct editing.
    #[test]
    fn event_field_actions_are_typed_strict_and_replayable() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
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

    #[test]
    fn new_task_action_creates_a_collaborative_task() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let spec = resources[0]
            .actions
            .iter()
            .find(|action| action.id == "new-task")
            .expect("new-task action");
        let before = CalDoc::read_model(&doc).tasks.len();
        let encoded = prompted_encode(spec, "Book dentist appointment");
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
        let blank = prompted_encode(spec, "\t ");
        assert_eq!(
            blank.type_id,
            tilo_actions::resource::INVALID_ARGUMENTS_TYPE,
            "new task refuses a whitespace-only title"
        );
    }

    #[test]
    fn projects_are_searchable_and_project_actions_preserve_relationships() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let linked_id = app
            .calendar_resource_ctx()
            .projects
            .iter()
            .find(|project| project.in_use)
            .expect("demo has a linked project")
            .data
            .id
            .clone();
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
            Widget::search_providers(&app)[0]
                .search(&name)
                .iter()
                .any(|hit| hit.action_address() == Some(open_project_address(id)))
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

    #[test]
    fn independent_project_fields_merge_and_history_keeps_the_project_identity() {
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

    #[test]
    fn invite_recurrence_and_timezone_actions_converge_across_divergent_replicas() {
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

    #[test]
    fn attendee_delete_tombstone_wins_over_stale_update() {
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

    #[test]
    fn task_field_actions_are_sparse_and_reject_hostile_priority_values() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
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

    #[test]
    fn import_batch_replays_as_one_deterministic_action() {
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
        let envelope = tilo_actions::apply_local(&mut a, &action, encoded).expect("apply import");
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
