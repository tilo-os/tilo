//! Calendar as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path for document state. View and effect operations live in
//! [`Resource::local_operations`]; entity children carry open/delete.

use std::rc::Rc;

use automerge::{AutoCommit, ChangeHash};
use tilo_actions::{ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType, RetryPolicy};
use tilo_document::{RichText, RichTextAction};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_resource::{
    ActionDecl, Doc, ModelError, Resource, SearchEntry, child_resource_id, publish_child,
};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{
    CALENDAR_ACTION_TYPE, CALENDAR_OPERATION_TYPE, CalendarAction, CalendarOperation, EventData,
    NoteOwner, ProjectData, TaskData, delete_event_action, delete_task_action, encode,
    set_task_done_action,
};
use crate::app::CalendarApp;
use crate::doc::CalDoc;
use crate::model::{Model, Palette, Priority};

fn default_local_event() -> EventData {
    EventData {
        id: String::new(),
        title: String::new(),
        start: "2026-01-01T09:00".into(),
        end: "2026-01-01T10:00".into(),
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
    }
}

/// The calendar resource — events, tasks and projects in one document.
pub struct Calendar;

/// The id an event's or a task's notes are published under, as a rich-text
/// child of the calendar.
pub(crate) fn notes_resource_id(owner: &NoteOwner) -> String {
    let (kind, id) = match owner {
        NoteOwner::Event { id } => ("event", id),
        NoteOwner::Task { id } => ("task", id),
    };
    child_resource_id::<Calendar>(&format!("{kind}:{id}/notes"))
}

/// Each edit of `owner`'s notes as the calendar's own action.
fn notes_edit(owner: NoteOwner) -> impl Fn(RichTextAction) -> CalendarAction + Send + Sync {
    move |edit| CalendarAction::EditNotes {
        owner: owner.clone(),
        edit,
    }
}

/// The dispatcher of `owner`'s notes: a rich-text child of the calendar
/// whose every edit is [`CalendarAction::EditNotes`] on that owner.
pub(crate) fn notes_dispatcher(
    calendar: &Dispatcher<Calendar>,
    owner: &NoteOwner,
) -> Dispatcher<RichText> {
    calendar.child::<RichText>(&notes_resource_id(owner), notes_edit(owner.clone()))
}

/// Every event's and task's notes, published as rich-text children.
fn publish_notes(state: &Model) -> impl Iterator<Item = tilo_actions::Resource> + '_ {
    let events = state.events.iter().map(|event| {
        let owner = NoteOwner::Event {
            id: event.id.to_string(),
        };
        (owner, event.display_title())
    });
    let tasks = state.tasks.iter().map(|task| {
        let owner = NoteOwner::Task {
            id: task.id.to_string(),
        };
        (owner, task.title.as_str())
    });
    events.chain(tasks).map(|(owner, of)| {
        publish_child::<RichText, Calendar>(
            &notes_resource_id(&owner),
            &format!("Notes of {of}"),
            None,
            notes_edit(owner),
        )
    })
}

/// Framework app: the calendar widget is the surface over [`Calendar`].
pub struct CalendarShell;

#[allow(
    clippy::expect_used,
    reason = "compile-time CalendarAction/Operation payloads always serialize"
)]
impl Resource for Calendar {
    const KIND: &'static str = "calendar.calendar";
    const LABEL: &'static str = "Calendar";
    const ACTION_TYPE: &'static str = CALENDAR_ACTION_TYPE;

    type State = Model;
    type Action = CalendarAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        crate::actions::register(registry);
    }

    fn read(doc: &Doc) -> Result<Model, ModelError> {
        Ok(CalDoc::read_model(doc))
    }

    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        // An empty Automerge document is a valid empty calendar; maps are
        // created on first write inside Action::apply.
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "new-event",
                "New event",
                vec![ActionParam::required(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "Title",
                )],
                |args, _| {
                    let title = args
                        .text(tilo_actions::PROMPTED_PARAM)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    let mut data = default_local_event();
                    data.id = Uuid::new_v4().to_string();
                    data.title = title;
                    Ok(CalendarAction::UpsertEvent(data))
                },
            )
            .describe("Create an event on the local calendar."),
            ActionDecl::new(
                "new-task",
                "New task",
                vec![ActionParam::required(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "Title",
                )],
                |args, _| {
                    let title = args
                        .text(tilo_actions::PROMPTED_PARAM)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    if title.is_empty() {
                        return Err("task title must contain non-whitespace text".into());
                    }
                    Ok(CalendarAction::UpsertTask(TaskData {
                        id: Uuid::new_v4().to_string(),
                        title,
                        done: false,
                        due: String::new(),
                        priority: Priority::None.rank(),
                        project: String::new(),
                        parent: String::new(),
                    }))
                },
            ),
            ActionDecl::new(
                "new-project",
                "New task project",
                vec![ActionParam::required(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "Name",
                )],
                |args, _| {
                    let name = args
                        .text(tilo_actions::PROMPTED_PARAM)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    if name.is_empty() {
                        return Err("project name must contain non-whitespace text".into());
                    }
                    Ok(CalendarAction::UpsertProject(ProjectData {
                        id: Uuid::new_v4().to_string(),
                        name,
                        color: Palette::Indigo.index(),
                        order: 0,
                    }))
                },
            ),
        ]
    }

    fn local_operations() -> Vec<ActionSpec> {
        let mut ops = vec![
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
                    EncodedAction::encode_operation(CALENDAR_OPERATION_TYPE, &operation)
                        .unwrap_or_else(|_| EncodedAction {
                            type_id: CALENDAR_OPERATION_TYPE.to_string(),
                            payload: Vec::new(),
                        })
                },
            )
            .retry_policy(RetryPolicy::Idempotent),
        ];
        for (id, label, operation) in [
            (
                "show-all-calendars",
                "Show all calendars",
                CalendarOperation::ShowAllCalendars,
            ),
            ("show-month", "Show month", CalendarOperation::ShowMonth),
            ("show-week", "Show week", CalendarOperation::ShowWeek),
            ("show-day", "Show day", CalendarOperation::ShowDay),
            ("show-agenda", "Show agenda", CalendarOperation::ShowAgenda),
            ("sync", "Sync calendars", CalendarOperation::Sync),
            ("export-ics", "Export ICS", CalendarOperation::ExportIcs),
            (
                "import-latest-ics",
                "Import latest ICS",
                CalendarOperation::ImportIcs,
            ),
        ] {
            let kind = match operation {
                CalendarOperation::Sync
                | CalendarOperation::ExportIcs
                | CalendarOperation::ImportIcs => OperationKind::Effect,
                _ => OperationKind::View,
            };
            ops.push(
                ActionSpec::operation(id, label, kind, CALENDAR_OPERATION_TYPE, &operation)
                    .expect("CalendarOperation encodes")
                    .retry_policy(RetryPolicy::Idempotent),
            );
        }
        ops
    }

    fn children(state: &Model) -> Vec<tilo_actions::Resource> {
        let mut children = Vec::new();
        for event in &state.events {
            let id = event.id.to_string();
            children.push(
                tilo_actions::Resource::new(
                    format!("calendar.event:{id}"),
                    "calendar.event",
                    event.display_title().to_owned(),
                )
                .with_action(
                    ActionSpec::operation(
                        "open",
                        "Open event",
                        OperationKind::View,
                        CALENDAR_OPERATION_TYPE,
                        &CalendarOperation::OpenEvent { id: id.clone() },
                    )
                    .expect("encodes")
                    .retry_policy(RetryPolicy::Idempotent),
                )
                .with_action(
                    ActionSpec::immediate(
                        "delete",
                        "Delete event",
                        CALENDAR_ACTION_TYPE,
                        &delete_event_action(&id, None),
                    )
                    .expect("encodes")
                    .reversible()
                    .destructive(),
                ),
            );
        }
        for task in &state.tasks {
            let id = task.id.to_string();
            let done = task.done;
            children.push(
                tilo_actions::Resource::new(
                    format!("calendar.task:{id}"),
                    "calendar.task",
                    task.title.clone(),
                )
                .with_action(
                    ActionSpec::operation(
                        "open",
                        "Open task",
                        OperationKind::View,
                        CALENDAR_OPERATION_TYPE,
                        &CalendarOperation::OpenTask { id: id.clone() },
                    )
                    .expect("encodes")
                    .retry_policy(RetryPolicy::Idempotent),
                )
                .with_action(
                    ActionSpec::immediate(
                        "toggle-done",
                        if done { "Mark not done" } else { "Mark done" },
                        CALENDAR_ACTION_TYPE,
                        &set_task_done_action(&id, !done),
                    )
                    .expect("encodes")
                    .reversible(),
                )
                .with_action(
                    ActionSpec::immediate(
                        "delete",
                        "Delete task",
                        CALENDAR_ACTION_TYPE,
                        &delete_task_action(&id, Vec::new()),
                    )
                    .expect("encodes")
                    .reversible()
                    .destructive(),
                ),
            );
        }
        children.extend(publish_notes(state));
        for project in &state.projects {
            let id = project.id.to_string();
            children.push(
                tilo_actions::Resource::new(
                    format!("calendar.project:{id}"),
                    "calendar.project",
                    project.name.clone(),
                )
                .with_action(
                    ActionSpec::operation(
                        "open",
                        "Open project tasks",
                        OperationKind::View,
                        CALENDAR_OPERATION_TYPE,
                        &CalendarOperation::OpenProject { id },
                    )
                    .expect("encodes")
                    .retry_policy(RetryPolicy::Idempotent),
                ),
            );
        }
        children
    }

    fn search(state: &Model) -> Vec<SearchEntry> {
        let events = state.events.iter().map(|event| SearchEntry {
            anchor: Some(format!("calendar.event:{}", event.id)),
            title: event.display_title().to_owned(),
            text: event.location.clone(),
            action: None,
        });
        let tasks = state.tasks.iter().map(|task| SearchEntry {
            anchor: Some(format!("calendar.task:{}", task.id)),
            title: task.title.clone(),
            text: String::new(),
            action: None,
        });
        events.chain(tasks).collect()
    }
}

impl ResourceApp for CalendarShell {
    type Resource = Calendar;
    const ID: &'static str = "so.tilo.calendar";
    const TITLE: &'static str = "Calendar";
    const ICON: Option<&'static str> = Some("calendar");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn view(cx: &mut ViewCx<Calendar>) -> Element {
        cx.surface(CalendarApp::new())
    }
}

/// Framework dispatcher when hosted; local action host for standalone tests.
#[derive(Default)]
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Calendar>>,
    last: Option<EncodedAction>,
    /// Test-only: hosted lookup without a framework dispatcher.
    #[cfg(test)]
    hosted: bool,
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Calendar>) {
        self.framework = Some(dispatcher);
    }

    /// The framework's dispatcher, once the calendar is hosted.
    pub(crate) fn framework(&self) -> Option<&Dispatcher<Calendar>> {
        self.framework.as_ref()
    }

    pub(crate) fn is_hosted(&self) -> bool {
        #[cfg(test)]
        if self.hosted {
            return true;
        }
        self.framework.is_some()
    }

    /// Test-only: mark hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &CalendarAction) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(encode(action));
                let _ =
                    dispatcher.apply_now_with(doc, action, crate::actions::history_context(action));
            }
            None => {
                self.local.dispatch_with_context(
                    doc,
                    CALENDAR_ACTION_TYPE,
                    action,
                    crate::actions::history_context(action),
                );
            }
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<ChangeHash>> {
        self.local.take_committed()
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &CalendarAction,
        context: tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(encode(action));
                dispatcher.apply_now_with(doc, action, context)
            }
            None => self
                .local
                .try_dispatch_with_context(doc, type_id, action, context),
        }
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}

impl ResourceSurface<Calendar> for CalendarApp {
    fn present(&mut self, state: &Rc<Model>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Calendar>) {
        self.attach_framework(cx.dispatcher());
    }
}

#[cfg(test)]
mod host_undo_tests {
    use super::*;
    use crate::model::Id;
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::Invoke;

    fn notes_of(app: &TestApp<CalendarShell>, event: Id) -> String {
        app.read_document(|doc| CalDoc::note_text(doc, true, event))
    }

    /// Typing in an open event's notes edits them as the calendar's
    /// `EditNotes`, named in history after the notes, a word per undo step,
    /// and kept when the calendar is opened again.
    #[test]
    fn typing_in_an_events_notes_edits_them_through_the_calendar() {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        app.harness().key("n");
        let event = app.state().events.first().expect("n creates an event").id;
        // The new event opens with its title focused: Tab moves on to the
        // location, the invite field, then the notes.
        for _ in 0..3 {
            app.harness().key("Tab");
        }
        app.harness().type_text("Agenda items");
        assert_eq!(notes_of(&app, event), "Agenda items");

        let notes = notes_resource_id(&NoteOwner::Event {
            id: event.to_string(),
        });
        assert!(
            app.versions()
                .into_iter()
                .filter_map(|version| version.context)
                .any(|context| context
                    .resource
                    .is_some_and(|resource| resource.id == notes)),
            "history names the event's notes as the resource edited"
        );
        assert!(app.undo());
        assert_eq!(notes_of(&app, event), "Agenda ");
        assert!(app.redo());

        let app = app.reopen().expect("reopen");
        assert_eq!(notes_of(&app, event), "Agenda items");
    }

    /// The palette, agents and scripts reach an event's notes as a rich-text
    /// resource of their own, and what they do is the calendar's edit.
    #[test]
    fn an_agent_edits_an_events_notes_as_a_rich_text_child() {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        let mut data = default_local_event();
        data.id = Uuid::new_v4().to_string();
        data.title = "Review".into();
        let event = data.id.parse::<Id>().expect("id");
        app.invoke(Invoke::apply(&CalendarAction::UpsertEvent(data)).expect("encode"))
            .expect("create");
        let notes = notes_resource_id(&NoteOwner::Event {
            id: event.to_string(),
        });
        let surface = app.agent_surface();
        assert!(
            surface.as_array().expect("rows").iter().any(|row| {
                row["resource"] == notes.as_str() && row["resource_kind"] == RichText::KIND
            }),
            "the notes are published as rich text: {surface}"
        );
        app.invoke_child::<RichText>(
            &notes,
            Invoke::<RichText>::apply(&RichTextAction::AppendParagraph {
                text: "Bring the mockups".into(),
            })
            .expect("encode"),
        )
        .expect("append");
        assert_eq!(notes_of(&app, event), "Bring the mockups");
        assert!(app.undo());
        assert_eq!(notes_of(&app, event), "");
    }

    /// Creating a remote-calendar event, then undoing through the host, must
    /// queue a `CalDAV` delete via the inverse document action — not Automerge
    /// patches alone.
    #[test]
    fn undo_of_remote_event_create_queues_del_outbox_through_host() {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        let remote = "work/team".to_owned();
        let mut data = default_local_event();
        data.id = Uuid::new_v4().to_string();
        data.title = "Standup".into();
        data.start = "2026-09-08T09:00".into();
        data.end = "2026-09-08T09:30".into();
        data.calendar_id = remote.clone();
        let id = data.id.parse::<Id>().expect("id");

        app.invoke(Invoke::apply(&CalendarAction::UpsertEvent(data)).expect("encode"))
            .expect("create");
        assert!(app.state().event(id).is_some());
        assert!(
            app.read_document(|doc| CalDoc::outbox(doc).contains(&id)),
            "remote create records the push outbox"
        );

        // Sync's Effect drained the push queue after a successful PUT.
        app.merge_peer_edit(|doc| {
            CalDoc::dequeue_push(doc, id);
        });
        assert!(app.read_document(|doc| CalDoc::delete_outbox(doc).is_empty()));

        assert!(app.undo(), "host undo applies the inverse action");
        assert!(app.state().event(id).is_none());
        assert!(
            app.read_document(|doc| {
                CalDoc::delete_outbox(doc)
                    .iter()
                    .any(|(cal, uid)| cal == &remote && uid == &id)
            }),
            "host undo queued a CalDAV delete in del_outbox"
        );
    }
}
