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
//! its top-level tasks, and each exports the [`ActionSpec`]s Ctrl+K offers.
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
    Action, ActionError, ActionInput, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, Resource, ResourceProvider,
};

use crate::doc::CalDoc;
use crate::model::{Event, Id, Palette, Priority, Recurrence, Task};

/// Registry `type_id` for every calendar action.
pub const CALENDAR_ACTION_TYPE: &str = "calendar.action";

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
            reminder: e.reminder_min.unwrap_or(-1),
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
        Some(Event {
            id,
            title: self.title.clone(),
            start,
            end,
            all_day: self.all_day,
            location: self.location.clone(),
            color: Palette::from_index(self.color),
            recurrence: Recurrence::from_rrule(&self.rrule),
            reminder_min: if self.reminder < 0 {
                None
            } else {
                Some(self.reminder)
            },
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
        Some(Task {
            id,
            title: self.title.clone(),
            done: self.done,
            due: if self.due.is_empty() {
                None
            } else {
                NaiveDate::parse_from_str(&self.due, DATE_FMT).ok()
            },
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
    /// Retarget an event to a calendar: write the (already resolved) event
    /// fields and update the `CalDAV` push queue — enqueue for a remote target,
    /// dequeue for the local one. The colour is already folded into `event`.
    SetEventCalendar { event: EventData, enqueue: bool },
    /// Delete an event, clearing any queued upload. When the event was synced
    /// from a still-configured remote calendar, `remote_delete` carries that
    /// calendar id so `apply` also queues a `CalDAV` delete of the server copy.
    DeleteEvent {
        id: String,
        remote_delete: Option<String>,
    },
    /// Create or replace a task's scalar fields.
    UpsertTask(TaskData),
    /// Flip a task's `done` flag. Deterministic against the replay base: the
    /// current value is read from the document and negated, so two replicas at
    /// the same heads land on the same result.
    ToggleTask { id: String },
    /// Delete a task and each of its listed subtasks (resolved at authoring
    /// time from the model, carried here so `apply` needs no lookup).
    DeleteTask { id: String, subtasks: Vec<String> },
}

/// Durable, browseable history metadata for a calendar mutation.
pub fn history_context(action: &CalendarAction) -> qos_actions::ActionContext {
    let mut context =
        qos_actions::ActionContext::for_action("org.qos.Calendar", "Calendar", action);
    let (label, id, kind, title) = match action {
        CalendarAction::UpsertEvent(event) => {
            ("Update event", &event.id, "calendar.event", &event.title)
        }
        CalendarAction::SetEventCalendar { event, .. } => {
            ("Move event", &event.id, "calendar.event", &event.title)
        }
        CalendarAction::DeleteEvent { id, .. } => ("Delete event", id, "calendar.event", id),
        CalendarAction::UpsertTask(task) => ("Update task", &task.id, "calendar.task", &task.title),
        CalendarAction::ToggleTask { id } => ("Toggle task", id, "calendar.task", id),
        CalendarAction::DeleteTask { id, .. } => ("Delete task", id, "calendar.task", id),
    };
    context.action_label = label.to_string();
    context.resource = Some(qos_actions::ActionResource {
        id: format!("{kind}:{id}"),
        kind: kind.to_string(),
        label: title.clone(),
    });
    context
}

impl Action for CalendarAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            CalendarAction::UpsertEvent(data) => {
                if let Some(e) = data.to_event() {
                    CalDoc::update_event(doc, &e);
                }
            }
            CalendarAction::SetEventCalendar { event, enqueue } => {
                if let Some(e) = event.to_event() {
                    let id = e.id;
                    CalDoc::update_event(doc, &e);
                    if *enqueue {
                        CalDoc::enqueue_push(doc, id);
                    } else {
                        CalDoc::dequeue_push(doc, id);
                    }
                }
            }
            CalendarAction::DeleteEvent { id, remote_delete } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    CalDoc::delete_event(doc, uuid);
                    // A queued upload for a now-deleted event is moot.
                    CalDoc::dequeue_push(doc, uuid);
                    if let Some(cal) = remote_delete {
                        CalDoc::enqueue_delete(doc, cal, uuid);
                    }
                }
            }
            CalendarAction::UpsertTask(data) => {
                if let Some(t) = data.to_task() {
                    CalDoc::update_task(doc, &t);
                }
            }
            CalendarAction::ToggleTask { id } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    CalDoc::toggle_task_done(doc, uuid);
                }
            }
            CalendarAction::DeleteTask { id, subtasks } => {
                if let Ok(uuid) = id.parse::<Id>() {
                    CalDoc::delete_task(doc, uuid);
                }
                for s in subtasks {
                    if let Ok(uuid) = s.parse::<Id>() {
                        CalDoc::delete_task(doc, uuid);
                    }
                }
            }
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
    CalendarAction::SetEventCalendar {
        event: EventData::from_event(e),
        enqueue,
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

/// One top-level task surfaced as a resource, with its subtask ids resolved so
/// "Delete task" removes the whole subtree.
#[derive(Debug, Clone)]
pub struct TaskResource {
    pub data: TaskData,
    pub subtasks: Vec<String>,
}

/// The presentation context the calendar hands [`provide_resources`]: a template
/// for a palette-created event (built from the current view) plus the events and
/// top-level tasks currently in the model. Everything here is derived from app
/// state the document already holds; nothing is fabricated.
#[derive(Debug, Clone)]
pub struct CalendarResourceCtx {
    /// The event a palette "New event" starts from — its start/end/colour come
    /// from the current view; its id and title are filled at commit time.
    pub new_event: EventData,
    pub events: Vec<EventResource>,
    pub tasks: Vec<TaskResource>,
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
    ActionSpec::immediate(id, label, CALENDAR_ACTION_TYPE, action).unwrap_or_else(|_| ActionSpec {
        id: id.to_string(),
        label: label.to_string(),
        description: None,
        locus: action.locus(),
        input: ActionInput::Immediate(encode(action)),
    })
}

/// Build the resource tree the calendar currently has open: the calendar itself
/// (with a "New event" action), each event (rename + delete), and each top-level
/// task (toggle + delete). Every document-mutating operation is exported as the
/// same action the on-screen affordance dispatches.
pub fn provide_resources(ctx: &CalendarResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("calendar.calendar", "calendar.calendar", "Calendar");

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

    for ev in &ctx.events {
        let id = ev.data.id.clone();
        let mut child = Resource::new(
            format!("calendar.event:{id}"),
            "calendar.event",
            event_label(&ev.data),
        );
        // Rename — a prompted action carrying the rest of the event unchanged.
        let data = ev.data.clone();
        child = child.with_action(ActionSpec::prompted(
            "edit",
            "Rename event",
            "Title",
            "event title…",
            ExecutionLocus::ClientLocal,
            move |title: &str| {
                let mut d = data.clone();
                d.title = title.trim().to_string();
                encode(&CalendarAction::UpsertEvent(d))
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
        let toggle_label = if tk.data.done {
            "Mark not done"
        } else {
            "Mark done"
        };
        let child = Resource::new(
            format!("calendar.task:{id}"),
            "calendar.task",
            task_label(&tk.data),
        )
        .with_action(immediate_spec(
            "toggle",
            toggle_label,
            &toggle_task_action(id.clone()),
        ))
        .with_action(immediate_spec(
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
    /// action, an `calendar.event` child per event exporting rename + delete, and
    /// a `calendar.task` child per top-level task exporting toggle + delete.
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
        assert!(ev_actions.contains(&"edit"), "{ev_actions:?}");
        assert!(ev_actions.contains(&"delete"), "{ev_actions:?}");

        let task = cal
            .children
            .iter()
            .find(|r| r.kind == "calendar.task")
            .expect("a task resource");
        let tk_actions: Vec<&str> = task.actions.iter().map(|a| a.id.as_str()).collect();
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
        assert!(
            labels.iter().any(|l| l.contains("Delete event")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Rename event")),
            "{labels:?}"
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

        // And the rename spec matches the upsert builder for the same title.
        let mut ev = CalDoc::read_model(&doc)
            .event(id.parse().unwrap())
            .unwrap()
            .clone();
        let edit_spec = event.actions.iter().find(|a| a.id == "edit").unwrap();
        let from_spec = prompted_encode(edit_spec, "New title");
        ev.title = "New title".into();
        let from_builder = encode(&upsert_event_action(&ev));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "rename is one source of truth"
        );
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
}
