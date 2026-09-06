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
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use qos_actions::{
    Action, ActionAddress, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, OperationKind, ParamType, Resource, ResourceProvider,
    RetryPolicy,
};

use crate::doc::CalDoc;
use crate::model::{Event, Id, Palette, Priority, Recurrence, Task};

/// Registry `type_id` for every calendar action.
pub const CALENDAR_ACTION_TYPE: &str = "calendar.action";
pub const CALENDAR_OPERATION_TYPE: &str = "calendar.operation";

/// Stable resource id for one event. Search, the palette, accessibility and
/// agents all address this same resource.
pub fn event_resource_id(id: Id) -> String {
    format!("calendar.event:{id}")
}

/// Stable resource id for one task.
pub fn task_resource_id(id: Id) -> String {
    format!("calendar.task:{id}")
}

/// Canonical view action that reveals one event in the calendar.
pub fn open_event_address(id: Id) -> ActionAddress {
    ActionAddress::new(event_resource_id(id), "open")
}

/// Canonical view action that reveals one task in the calendar.
pub fn open_task_address(id: Id) -> ActionAddress {
    ActionAddress::new(task_resource_id(id), "open")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalendarOperation {
    OpenEvent {
        id: String,
    },
    OpenTask {
        id: String,
    },
    ShowMonth,
    ShowWeek,
    ShowDay,
    ShowAgenda,
    Sync,
    ExportIcs,
    /// Import the newest `.ics` file from the transfer directory. The effect
    /// captures parsed records into one [`CalendarAction`] before it reports
    /// success; its retry contract is therefore at-most-once.
    ImportIcs,
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
        | CalendarOperation::OpenTask { .. }
        | CalendarOperation::ShowMonth
        | CalendarOperation::ShowWeek
        | CalendarOperation::ShowDay
        | CalendarOperation::ShowAgenda => spec.retry_policy(RetryPolicy::Idempotent),
        CalendarOperation::Sync => spec
            .retry_policy(RetryPolicy::Idempotent)
            .execution(qos_actions::ExecutionMode::Background { cancellable: false }),
        CalendarOperation::ExportIcs | CalendarOperation::ImportIcs => {
            spec.retry_policy(RetryPolicy::AtMostOnce)
        }
    }
}

/// Encode an app-local operation without allowing a serialization fault to
/// crash a desktop application. The enum only contains strings and unit
/// variants today, but the fallback keeps its failure mode explicit if that
/// changes in the future.
fn encode_operation(operation: &CalendarOperation) -> EncodedAction {
    EncodedAction::encode_operation(CALENDAR_OPERATION_TYPE, operation).unwrap_or_else(|_| {
        EncodedAction {
            type_id: qos_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
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
    /// Minutes before start for a reminder, or `-1` for none.
    pub reminder: i64,
    /// Owning project id (string form), or `""` for none.
    pub project: String,
    /// Source calendar id, or `""` for the local on-device calendar.
    pub calendar_id: String,
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
        let start = NaiveDateTime::parse_from_str(&self.start, DT_FMT).ok()?;
        let end = NaiveDateTime::parse_from_str(&self.end, DT_FMT).ok()?;
        if start > end
            || self.color >= Palette::ALL.len()
            || !matches!(self.reminder, -1 | 10 | 30 | 60)
            || Recurrence::from_rrule(&self.rrule).to_rrule() != self.rrule
            || (!self.project.is_empty() && self.project.parse::<Id>().is_err())
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
    pub reminder: Option<i64>,
    /// Project id, or an empty string to remove the project association.
    pub project: Option<String>,
    pub calendar_id: Option<String>,
    /// Only calendar moves affect the remote outbox. `Some(true)` queues a
    /// remote upload; `Some(false)` removes a now-local event from it.
    pub enqueue_remote: Option<bool>,
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
    /// Create or replace an event's scalar fields. One variant covers create,
    /// every field edit, and a move (a start/end change is just new datetimes).
    UpsertEvent(EventData),
    /// Update only named event registers. This is what all ordinary editor and
    /// agent field edits use; `UpsertEvent` is reserved for creation/import.
    PatchEvent { id: String, patch: EventPatch },
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
    /// Flip a task's `done` flag. Deterministic against the replay base: the
    /// current value is read from the document and negated, so two replicas at
    /// the same heads land on the same result.
    ToggleTask { id: String },
    /// Delete a task and each of its listed subtasks (resolved at authoring
    /// time from the model, carried here so `apply` needs no lookup).
    DeleteTask { id: String, subtasks: Vec<String> },
    /// A single deterministic import batch. The external file is parsed before
    /// this action is authored; replay only writes the exact payload captured
    /// here, and has one history/undo boundary.
    ImportIcs {
        events: Vec<EventData>,
        tasks: Vec<TaskData>,
    },
}

/// Durable, browseable history metadata for a calendar mutation.
pub fn history_context(action: &CalendarAction) -> qos_actions::ActionContext {
    let mut context =
        qos_actions::ActionContext::for_action("org.qos.Calendar", "Calendar", action);
    let (label, id, kind, title) = match action {
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
        CalendarAction::ToggleTask { id } => {
            ("Toggle task", id.clone(), "calendar.task", id.clone())
        }
        CalendarAction::DeleteTask { id, .. } => {
            ("Delete task", id.clone(), "calendar.task", id.clone())
        }
        CalendarAction::ImportIcs { .. } => (
            "Import calendar",
            "calendar".into(),
            "calendar.calendar",
            "Calendar".into(),
        ),
    };
    context.action_label = label.to_string();
    context.resource = Some(qos_actions::ActionResource {
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

fn apply_toggle_task(doc: &mut Doc, id: &str) -> Result<(), ActionError> {
    let id = id
        .parse::<Id>()
        .map_err(|_| refused("task id is invalid"))?;
    if CalDoc::read_model(doc).task(id).is_none() {
        return Err(refused("task no longer exists"));
    }
    CalDoc::toggle_task_done(doc, id);
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

fn apply_import_ics(doc: &mut Doc, events: &[EventData], tasks: &[TaskData]) {
    for data in events {
        if let Some(event) = data.to_event()
            && CalDoc::project_exists(doc, event.project)
        {
            CalDoc::update_event(doc, &event);
            if let Some(notes) = CalDoc::event_notes(doc, event.id) {
                CalDoc::seed_notes_if_empty(doc, &notes, &["Imported from .ics"]);
            }
        }
    }
    for data in tasks {
        apply_upsert_task(doc, data);
    }
}

impl Action for CalendarAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            CalendarAction::UpsertEvent(data) => apply_upsert_event(doc, data),
            CalendarAction::PatchEvent { id, patch } => apply_patch_event(doc, id, patch)?,
            CalendarAction::DeleteEvent { id, remote_delete } => {
                apply_delete_event(doc, id, remote_delete.as_ref())?;
            }
            CalendarAction::UpsertTask(data) => apply_upsert_task(doc, data),
            CalendarAction::PatchTask { id, patch } => apply_patch_task(doc, id, patch)?,
            CalendarAction::ToggleTask { id } => apply_toggle_task(doc, id)?,
            CalendarAction::DeleteTask { id, subtasks } => apply_delete_task(doc, id, subtasks)?,
            CalendarAction::ImportIcs { events, tasks } => apply_import_ics(doc, events, tasks),
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

/// Toggle a task's completion.
pub fn toggle_task_action(id: impl Into<String>) -> CalendarAction {
    CalendarAction::ToggleTask { id: id.into() }
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

/// One task surfaced as a resource, with its subtask ids resolved so deleting a
/// parent removes the whole subtree. Subtasks are resources too: search and
/// agents must be able to address every task the app presents.
#[derive(Debug, Clone)]
pub struct TaskResource {
    pub data: TaskData,
    pub subtasks: Vec<String>,
}

/// A destination that an event may be filed under. This is projected from the
/// shared account store at resource-publication time, so an agent gets the
/// same real target list as the editor's calendar picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarTargetResource {
    /// `local` for the on-device calendar, otherwise the stable remote
    /// collection id carried by Calendar accounts.
    pub id: String,
    pub label: String,
    /// The selected remote calendar's palette index. Local does not impose a
    /// colour, matching the visible editor behaviour.
    pub color: Option<usize>,
}

/// A real local project that a task may be filed under. Projects live in the
/// shared calendar document, so their ids are safe collaboration targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskProjectTarget {
    pub id: String,
    pub label: String,
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
    pub calendar_targets: Vec<CalendarTargetResource>,
    pub task_project_targets: Vec<TaskProjectTarget>,
    pub can_sync: bool,
    pub syncing: bool,
    pub can_export: bool,
}

/// Return an explicit invalid-arguments envelope rather than silently turning
/// a malformed agent request into a no-op document action.
fn invalid_arguments(reason: impl Into<String>) -> EncodedAction {
    EncodedAction {
        type_id: qos_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
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

/// Build the resource tree the calendar currently has open: the calendar itself
/// (with a "New event" action), each event (open + rename + delete), and each
/// task (open + toggle + delete). Every document-mutating operation is exported as the
/// same action the on-screen affordance dispatches.
#[allow(
    clippy::too_many_lines,
    reason = "resource publication intentionally keeps every event and task action beside its typed validator"
)]
pub fn provide_resources(ctx: &CalendarResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
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

    for ev in &ctx.events {
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
        let targets = ctx.calendar_targets.clone();
        child = child.with_action(event_update_spec(
            "move-to-calendar",
            "Move to calendar",
            "calendar destination",
            vec![
                ActionParam::required(
                    "calendar",
                    ParamType::Text,
                    "The local or configured remote calendar that should own this event",
                )
                .with_choices(targets.iter().map(|target| target.id.clone())),
            ],
            ev.data.clone(),
            move |data, args| {
                let id = args.text("calendar").unwrap_or_default();
                let target = targets
                    .iter()
                    .find(|target| target.id == id)
                    .ok_or_else(|| "calendar is no longer configured".to_string())?;
                if target.id == "local" {
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
                            calendar_id: Some(target.id.clone()),
                            color: target.color,
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
        root = root.with_child(child);
    }

    for tk in &ctx.tasks {
        let id = tk.data.id.clone();
        let Ok(parsed_id) = id.parse::<Id>() else {
            continue;
        };
        let toggle_label = if tk.data.done {
            "Mark not done"
        } else {
            "Mark done"
        };
        let projects = ctx.task_project_targets.clone();
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
            &toggle_task_action(id.clone()),
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
            vec![
                ActionParam::required("project", ParamType::Text, "A Calendar project or none")
                    .with_choices(
                        std::iter::once("none".to_string())
                            .chain(projects.iter().map(|project| project.id.clone())),
                    ),
            ],
            tk.data.clone(),
            move |data, args| {
                let selected = args.text("project").unwrap_or_default();
                let project = if selected == "none" {
                    String::new()
                } else if projects.iter().any(|project| project.id == selected) {
                    selected.to_string()
                } else {
                    return Err("project is no longer available".into());
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
        child = child.with_action(immediate_spec(
            "delete",
            "Delete task",
            &delete_task_action(id.clone(), tk.subtasks.clone()),
        ));
        root = root.with_child(child);
    }

    vec![root]
}

impl ResourceProvider for crate::app::CalendarApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        provide_resources(&self.calendar_resource_ctx(), doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::CalendarApp;
    use automerge::ROOT;
    use qos_actions::{ActionInput, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
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

        let event = cal
            .children
            .iter()
            .find(|r| r.kind == "calendar.event")
            .expect("an event resource");
        let ev_actions: Vec<&str> = event.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ev_actions.contains(&"open"), "{ev_actions:?}");
        assert!(ev_actions.contains(&"edit"), "{ev_actions:?}");
        assert!(ev_actions.contains(&"delete"), "{ev_actions:?}");

        let task = cal
            .children
            .iter()
            .find(|r| r.kind == "calendar.task")
            .expect("a task resource");
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
        let flat: Vec<&str> = qos_actions::flatten(&resources)
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

    /// Search activation stays entirely inside the resource/action door for
    /// both content types and invokes the exact exported view operation.
    #[test]
    fn search_rows_invoke_exported_event_and_task_view_actions() {
        let (mut app, doc) = seeded();

        let resources = Widget::resources(&app, &doc);
        let all_hits = Widget::search_providers(&app)[0].search("");
        assert!(
            all_hits.iter().all(|hit| hit
                .action_address()
                .is_some_and(|address| qos_actions::door::resolve(&resources, &address).is_some())),
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
                    &operation.encoded.type_id,
                    &operation.encoded.payload,
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
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
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

        // Toggle a task: deterministic against the shared base (each replica reads
        // the stored value and negates it).
        let task = CalDoc::read_model(&a)
            .tasks
            .into_iter()
            .find(|t| !t.done)
            .unwrap();
        let action = toggle_task_action(task.id.to_string());
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

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

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

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
        let event = cal
            .children
            .iter()
            .find(|r| r.kind == "calendar.event")
            .unwrap();
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
            qos_actions::resource::INVALID_ARGUMENTS_TYPE,
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
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

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
        let event = resources[0]
            .children
            .iter()
            .find(|resource| resource.kind == "calendar.event")
            .expect("seeded event resource");
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
                qos_actions::ArgValue::Text("Studio 3 · https://meet.example".to_string()),
            )])
            .expect("well-typed location request");
        let decoded = registry().decode(&encoded).expect("calendar action");
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
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
                    qos_actions::ArgValue::Text("every-other-tuesday".to_string()),
                )])
                .is_err(),
            "the declared choices reject an invented recurrence rather than silently defaulting"
        );
        let encoded = repeat
            .build_args([(
                "repeat".to_string(),
                qos_actions::ArgValue::Text("weekly".to_string()),
            )])
            .expect("declared recurrence is accepted");
        let decoded = registry().decode(&encoded).expect("calendar action");
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
            .expect("repeat action dispatches");
        let updated = CalDoc::read_model(&doc).event(id).expect("event").clone();
        assert_eq!(updated.recurrence, Recurrence::Weekly);
        assert_eq!(
            updated.location, "Studio 3 · https://meet.example",
            "a recurrence patch did not rewrite an independently-authored location"
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
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
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
            qos_actions::resource::INVALID_ARGUMENTS_TYPE,
            "new task refuses a whitespace-only title"
        );
    }

    #[test]
    fn task_field_actions_are_sparse_and_reject_hostile_priority_values() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let task = resources[0]
            .children
            .iter()
            .find(|resource| resource.kind == "calendar.task")
            .expect("seeded task resource");
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
                qos_actions::ArgValue::Text("p1".to_string()),
            )])
            .expect("declared priority is accepted");
        let decoded = registry().decode(&encoded).expect("calendar action");
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx)
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
            reminder: -1,
            project: String::new(),
            calendar_id: String::new(),
        };
        let action = CalendarAction::ImportIcs {
            events: vec![event.clone()],
            tasks: Vec::new(),
        };
        let encoded = encode(&action);
        let envelope = qos_actions::apply_local(&mut a, &action, encoded).expect("apply import");
        qos_actions::replay(&mut b, &envelope, &registry()).expect("replay import");
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
