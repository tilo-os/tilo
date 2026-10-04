//! Calendar as a [`tilo_resource::Resource`]: one action vocabulary, one
//! publish path for document state. View and effect operations live in
//! [`Resource::local_operations`]; entity children carry open/delete.

use std::rc::Rc;

use tilo_actions::{ActionParam, ActionSpec, ParamType};
use tilo_document::{RichText, RichTextAction};
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Fixture, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_resource::{
    self, ActionDecl, Doc, Effect, Invoke, ModelError, Resource, ScopeDecl, SearchEntry,
    child_resource_id, publish_child,
};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{CALENDAR_ACTION_TYPE, CalendarAction, EventData, NoteOwner, ProjectData};
use crate::app::CalendarApp;
use crate::doc::CalDoc;
use crate::model::{Model, Palette};

/// The start of the next whole hour, where a new event lands unless it names
/// its own time.
fn next_hour() -> chrono::NaiveDateTime {
    use chrono::Timelike as _;
    let now = chrono::Local::now().naive_local();
    let hour = now.date().and_hms_opt(now.hour(), 0, 0).unwrap_or(now);
    hour + chrono::Duration::hours(1)
}

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
    events
        .chain(tasks)
        .map(|(owner, of)| notes_resource(owner, of))
}

fn notes_resource(owner: NoteOwner, of: &str) -> tilo_actions::Resource {
    publish_child::<RichText, Calendar>(
        &notes_resource_id(&owner),
        &format!("Notes of {of}"),
        None,
        notes_edit(owner),
    )
}

/// The notes published at `id`, when their event or task is still there.
fn notes_child(state: &Model, id: &str) -> Option<tilo_actions::Resource> {
    let name = id
        .strip_prefix(Calendar::KIND)?
        .strip_prefix('/')?
        .strip_suffix("/notes")?;
    let (kind, raw) = name.split_once(':')?;
    let item = raw.parse::<crate::model::Id>().ok()?;
    let (owner, of) = match kind {
        "event" => (
            NoteOwner::Event { id: raw.to_owned() },
            state.event(item)?.display_title(),
        ),
        "task" => (
            NoteOwner::Task { id: raw.to_owned() },
            state.task(item)?.title.as_str(),
        ),
        _ => return None,
    };
    Some(notes_resource(owner, of))
}

/// The calendar's work outside its document: talking to the servers of the
/// accounts on this device, files, Mail, and how this person shows each
/// calendar.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "calendar.effects")]
pub enum CalendarEffect {
    /// Sync calendars
    ///
    /// Pulls the calendars of this device's accounts from their servers.
    #[effect(job, device_only)]
    Sync,
    /// Export ICS
    ///
    /// Saves every event and task to an iCalendar file you choose.
    #[effect(job, device_only)]
    ExportIcs,
    /// Import ICS
    ///
    /// Imports the events and tasks of an iCalendar file you choose.
    #[effect(job, device_only)]
    ImportIcs,
    /// Prepare invitation in Mail
    ///
    /// Answers with the arguments of Mail's invitation draft for one attendee;
    /// nothing is sent.
    #[effect(returns, permission = "invite")]
    PrepareInMail {
        #[param("Event id")]
        event_id: String,
        #[param("Attendee id")]
        attendee_id: String,
    },
    /// Set a calendar's colour
    ///
    /// A display choice of this person, which follows them to their other
    /// devices.
    SetColor {
        #[param("Calendar id")]
        id: String,
        #[param("indigo, emerald, amber, red, blue, violet or slate")]
        color: String,
    },
    /// Show or hide a calendar
    ///
    /// A display choice of this person, which follows them to their other
    /// devices.
    SetVisibility {
        #[param("Calendar id")]
        id: String,
        #[param("Whether its events are shown")]
        visible: bool,
    },
    /// Show calendar view
    #[effect(view, idempotent)]
    ShowView {
        #[param("Calendar view to show", choices = ["month", "week", "day", "agenda", "tasks"])]
        view: String,
    },
    /// Show all calendars
    #[effect(view, idempotent)]
    ShowAllCalendars,
    /// Show month
    #[effect(view, idempotent)]
    ShowMonth,
    /// Show week
    #[effect(view, idempotent)]
    ShowWeek,
    /// Show day
    #[effect(view, idempotent)]
    ShowDay,
    /// Show agenda
    #[effect(view, idempotent)]
    ShowAgenda,
    /// Show tasks
    ///
    /// Opens this workspace's Inbox task list without changing its content.
    #[effect(view, idempotent, row)]
    ShowTasks,
    /// Open
    ///
    /// Shows an event, one occurrence of it, a task, a project's tasks or
    /// one calendar's events.
    #[effect(view, row, idempotent)]
    Open {
        #[param("The event, occurrence, task, project or calendar's resource id")]
        item: String,
    },
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
                vec![
                    ActionParam::required(tilo_actions::PROMPTED_PARAM, ParamType::Text, "Title"),
                    ActionParam::optional(
                        "start",
                        ParamType::Text,
                        "Local start date and time in YYYY-MM-DDTHH:MM (default: the next hour)",
                    ),
                    ActionParam::optional(
                        "end",
                        ParamType::Text,
                        "Local end date and time in YYYY-MM-DDTHH:MM (default: an hour after the start)",
                    ),
                ],
                |args, _| {
                    let title = args
                        .text(tilo_actions::PROMPTED_PARAM)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    let parse = |field: &str| {
                        args.text(field)
                            .map(|value| {
                                chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
                                    .map_err(|_| format!("{field} must be YYYY-MM-DDTHH:MM"))
                            })
                            .transpose()
                    };
                    let start = parse("start")?.unwrap_or_else(next_hour);
                    let end = parse("end")?.unwrap_or(start + chrono::Duration::hours(1));
                    if end < start {
                        return Err("end must not be before start".into());
                    }
                    let mut data = default_local_event();
                    data.id = Uuid::new_v4().to_string();
                    data.title = title;
                    data.start = start.format("%Y-%m-%dT%H:%M").to_string();
                    data.end = end.format("%Y-%m-%dT%H:%M").to_string();
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
                    crate::actions::new_task(
                        args.text(tilo_actions::PROMPTED_PARAM).unwrap_or_default(),
                        None,
                        None,
                    )
                    .map(|task| crate::actions::upsert_task_action(&task))
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
        vec![CalendarEffect::ShowTasks.bound().launchable_when_closed()]
    }

    /// A page of the calendar's projects, events and tasks with every edit
    /// they offer, and the notes of those events and tasks.
    fn children(state: &Model) -> Vec<tilo_actions::Resource> {
        let mut children = crate::tree::children(state);
        children.extend(publish_notes(state).take(crate::tree::PAGE));
        children
    }

    /// Search narrows to the events, to the tasks, or to one project's
    /// tasks; a project's scope nests under the tasks'.
    fn search_scopes(state: &Model) -> Vec<ScopeDecl> {
        let mut scopes = vec![
            ScopeDecl::new(EVENTS_SCOPE, "Events"),
            ScopeDecl::new(TASKS_SCOPE, "Tasks"),
        ];
        scopes.extend(
            state
                .projects
                .iter()
                .map(|project| ScopeDecl::new(project_scope(project.id), project.name.clone())),
        );
        scopes
    }

    fn child(state: &Model, id: &str) -> Option<tilo_actions::Resource> {
        crate::tree::child(state, id).or_else(|| notes_child(state, id))
    }

    /// Every event, task and project, found by its title, place or notes;
    /// choosing one opens it.
    fn search(state: &Model) -> Vec<SearchEntry> {
        let notes = |id| state.notes.get(&id).cloned().unwrap_or_default();
        let events = state.events.iter().map(|event| SearchEntry {
            within: Some(EVENTS_SCOPE.into()),
            anchor: Some(crate::actions::event_resource_id(event.id)),
            title: event.display_title().to_owned(),
            text: format!("{} {}", event.location, notes(event.id)),
            action: Some(crate::actions::open_event_address(event.id)),
            ..Default::default()
        });
        let tasks = state.tasks.iter().map(|task| SearchEntry {
            within: Some(
                task.project
                    .map_or_else(|| TASKS_SCOPE.to_owned(), project_scope),
            ),
            anchor: Some(crate::actions::task_resource_id(task.id)),
            title: task.title.clone(),
            text: notes(task.id),
            action: Some(crate::actions::open_task_address(task.id)),
            ..Default::default()
        });
        let projects = state.projects.iter().map(|project| SearchEntry {
            anchor: Some(crate::actions::project_resource_id(project.id)),
            title: project.name.clone(),
            text: String::new(),
            action: Some(crate::actions::open_project_address(project.id)),
            ..Default::default()
        });
        events.chain(tasks).chain(projects).collect()
    }
}

/// Every screenshot scenario: the five views with the editor closed, the
/// event editor and each of its popovers, the task detail and its pickers,
/// the empty and transport states, and the two narrow presentations.
const SCENARIOS: &[&str] = &[
    "month",
    "week",
    "day",
    "agenda",
    "tasks",
    "event_editor",
    "new_event",
    "date_popover",
    "time_popover",
    "calendar_popover",
    "task_detail",
    "task_due_picker",
    "task_priority_picker",
    "task_project_picker",
    "empty",
    "empty_week",
    "empty_agenda",
    "empty_tasks",
    "sync_error",
    "sync_offline",
    "sync_syncing",
    "sync_conflict",
    "collapsed_sidebar",
    "narrow_drawer",
    "narrow_editor",
];

/// Whether `scenario` shows a first run: no events, tasks or accounts.
fn is_empty_scenario(scenario: Option<&str>) -> bool {
    scenario.is_some_and(|name| name.starts_with("empty"))
}

/// What a capture shows in place of the person's `CalDAV` accounts: sample
/// accounts whose servers answer from bundled sample calendars, never the
/// network.
pub(crate) struct SampleAccounts(pub(crate) crate::AccountStore);

/// The moment a capture shows as now: the same day and time on every run,
/// so the sample calendar, today's highlight and the now line never move.
pub(crate) struct ShotNow(pub(crate) chrono::NaiveDateTime);

/// The day and time every capture is taken at: a Tuesday morning.
pub(crate) fn shot_now() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 6, 30)
        .and_then(|day| day.and_hms_opt(10, 30, 0))
        .unwrap_or_default()
}

/// The curated notes the sample calendar opens with, by the title their event
/// starts with.
const SAMPLE_NOTES: [(&str, &str); 2] = [
    (
        "Design review",
        "# Agenda\n\nWalk through the latest mockups.\n\nOpen questions:\n\n\
         Sign-off on the final layout?\n\nConfirm the launch timeline.",
    ),
    (
        "Team standup",
        "# Standup\n\nWrapped up last week's report.\n\n\
         Following up with the vendor today.\n\nBlockers: none.",
    ),
];

/// The actions that fill an empty calendar with `model`: its projects, then
/// its events and tasks, then the curated notes of the sample events.
pub(crate) fn sample_actions(model: &Model) -> Vec<CalendarAction> {
    let projects = model
        .projects
        .iter()
        .map(|project| CalendarAction::UpsertProject(ProjectData::from_project(project)));
    let events = model.events.iter().map(crate::actions::upsert_event_action);
    let tasks = model.tasks.iter().map(crate::actions::upsert_task_action);
    let notes = SAMPLE_NOTES.iter().filter_map(|(title, markdown)| {
        let event = model
            .events
            .iter()
            .find(|event| event.title.starts_with(title))?;
        Some(CalendarAction::EditNotes {
            owner: NoteOwner::Event {
                id: event.id.to_string(),
            },
            edit: RichTextAction::ReplaceBody {
                markdown: (*markdown).to_owned(),
            },
        })
    });
    projects.chain(events).chain(tasks).chain(notes).collect()
}

/// The `ICS` text a launch hands the calendar: the file a host handed over,
/// else the local file its argument names.
fn launched_ics(input: &LaunchInput<'_>) -> Result<Option<String>, String> {
    const LIMIT: usize = 8 * 1024 * 1024;
    let bytes = if let Some(file) = input.file() {
        file.bytes().to_vec()
    } else if let Some(target) = input.argument() {
        let path = local_path(target)?;
        let (dir, name) =
            tilo_files::file_at(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        tilo_files::blocking::read(&dir, &name)
            .map_err(|error| format!("{}: {error}", path.display()))?
    } else {
        return Ok(None);
    };
    if bytes.len() > LIMIT {
        return Err("a calendar file to import is at most 8 MiB".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "a calendar file to import is UTF-8 text".into())
}

/// The local path a launch argument names: a path, or a `file:` URL on this
/// machine.
fn local_path(target: &str) -> Result<std::path::PathBuf, String> {
    if target.to_ascii_lowercase().starts_with("file:") {
        let url = url::Url::parse(target).map_err(|error| format!("invalid file URL: {error}"))?;
        if url
            .host_str()
            .is_some_and(|host| !host.is_empty() && host != "localhost")
        {
            return Err("the calendar imports only files on this device".into());
        }
        Ok(std::path::PathBuf::from(ui::uri::decode(url.path())))
    } else if target.contains("://") {
        Err("the calendar imports only files on this device".into())
    } else {
        Ok(std::path::PathBuf::from(target))
    }
}

/// The search scope holding every event.
const EVENTS_SCOPE: &str = "calendar.events";
/// The search scope holding every task.
const TASKS_SCOPE: &str = "calendar.tasks";

/// The search scope of one project's tasks, inside the tasks' scope.
fn project_scope(id: crate::model::Id) -> String {
    format!("{TASKS_SCOPE}/calendar.project:{id}")
}

impl ResourceApp for CalendarShell {
    type Resource = Calendar;
    const ID: &'static str = "so.tilo.calendar";
    const TITLE: &'static str = "Calendar";
    const ICON: Option<&'static str> = Some("calendar");
    const OPENS: &'static [&'static str] = &["text/calendar"];
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Calendar>) -> Element {
        cx.surface(CalendarApp::new())
    }

    /// A calendar file the calendar is opened with is imported into it: the
    /// events and tasks it holds that the calendar does not, as one step.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Calendar>> {
        let source = match launched_ics(input) {
            Ok(Some(source)) => source,
            Ok(None) => return Vec::new(),
            Err(error) => {
                tracing::warn!(%error, "calendar: the launch file was not imported");
                return Vec::new();
            }
        };
        let action = crate::actions::import_action(&source);
        if action.is_none() {
            tracing::warn!("calendar: the launch file holds no supported calendar items");
        }
        action
            .and_then(|action| Invoke::apply(&action).ok())
            .into_iter()
            .collect()
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario.is_some_and(|name| name.starts_with("narrow_")) {
            (720, 900)
        } else {
            (1280, 800)
        }
    }

    fn fixture(fixture: &mut Fixture<Calendar>) {
        fixture.provide(ShotNow(shot_now()));
        if !is_empty_scenario(fixture.scenario()) {
            fixture.provide(SampleAccounts(crate::AccountStore::sample()));
        }
    }

    /// The sample calendar around the capture's day, unless the scenario is
    /// a first run.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Calendar>> {
        if is_empty_scenario(name) {
            return Vec::new();
        }
        let today = shot_now().date();
        sample_actions(&crate::model::seed_demo(today))
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }
}

impl ResourceSurface<Calendar, CalendarEffect> for CalendarApp {
    fn present(&mut self, state: &Rc<Model>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Calendar>) {
        if let Some(now) = cx.fixture::<ShotNow>() {
            self.pin_now(now.0);
        }
        let accounts = if let Some(sample) = cx.fixture::<SampleAccounts>() {
            Some(sample.0.clone())
        } else if cx.mode().is_live() {
            Some(crate::AccountStore::load_from_settings().unwrap_or_default())
        } else {
            None
        };
        self.follow_display(cx.settings::<crate::display::CalendarDisplay>());
        if cx.mode().is_live() {
            crate::outbox::drain(&cx, self.device_accounts());
        }
        self.attach_framework(
            cx.dispatcher(),
            accounts,
            cx.fixture::<SampleAccounts>().is_some(),
        );
        if cx.mode().is_live()
            && let Some(accounts) = cx.shared_person::<accounts::person::PersonAccounts>()
        {
            self.follow_person_accounts(accounts);
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: CalendarEffect,
        cx: &mut EffectCx<Calendar, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// The calendars of the accounts on this device, which can be opened,
    /// shown and hidden.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.collection_rows()
    }
}

#[cfg(test)]
mod search_scope_tests {
    use super::*;
    use ui::tilo_resource::scope_contains;

    /// Every entry lies in the events' or the tasks' scope, and a project's
    /// tasks also in that project's scope.
    #[test]
    fn entries_nest_under_the_declared_scopes() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 6, 30).expect("date");
        let model = crate::model::seed_demo(today);
        let scopes = Calendar::search_scopes(&model);
        let entries = Calendar::search(&model);
        assert_eq!(scopes.len(), 2 + model.projects.len());
        let in_scope = |anchor: &str| entries.iter().filter(|e| e.in_scope(anchor)).count();
        assert_eq!(in_scope(EVENTS_SCOPE), model.events.len());
        assert_eq!(in_scope(TASKS_SCOPE), model.tasks.len());
        for project in &model.projects {
            let scope = scopes
                .iter()
                .find(|scope| scope.label == project.name)
                .expect("each project is a scope");
            assert!(scope_contains(TASKS_SCOPE, &scope.anchor));
            let tasks = model
                .tasks
                .iter()
                .filter(|task| task.project == Some(project.id))
                .count();
            assert_eq!(in_scope(&scope.anchor), tasks);
        }
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

        // The upload drain acknowledged the push after a successful PUT.
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

    /// A calendar file named on the command line is imported as one step,
    /// and opening it again adds nothing the calendar already holds.
    #[test]
    fn a_calendar_file_the_app_is_opened_with_is_imported_once() {
        let directory = tempfile::tempdir().expect("directory");
        let source = directory.path().join("Kalender – Zürich 日本.ics");
        let payload = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:launch-event\r\n\
                       SUMMARY:Launch import\r\nDTSTART:20260919T090000\r\n\
                       DTEND:20260919T100000\r\nEND:VEVENT\r\nBEGIN:VTODO\r\n\
                       UID:launch-task\r\nSUMMARY:Launch task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        std::fs::write(&source, payload).expect("write");
        let path = source.to_str().expect("utf-8 path");

        let app = TestApp::<CalendarShell>::launched(&[path]).expect("launch");
        let state = app.state();
        assert_eq!(state.events.len(), 1);
        assert_eq!(state.events[0].title, "Launch import");
        assert_eq!(state.tasks.len(), 1);

        let url = url::Url::from_file_path(&source).expect("file URL");
        let app = app.relaunch(&[url.as_str()]).expect("relaunch");
        assert_eq!(
            app.state().events.len(),
            1,
            "the event is not imported twice"
        );
        assert_eq!(app.state().tasks.len(), 1);
        let app = app.reopen().expect("reopen");
        assert_eq!(app.state().events.len(), 1, "the import is kept");
        assert_eq!(std::fs::read_to_string(&source).expect("read"), payload);
    }

    /// A launch naming a remote URL, a folder or a file that is not
    /// calendar text opens the calendar unchanged.
    #[test]
    fn a_launch_naming_nothing_importable_changes_nothing() {
        let directory = tempfile::tempdir().expect("directory");
        let binary = directory.path().join("invalid.ics");
        std::fs::write(&binary, [0xff]).expect("write");
        for target in [
            "https://example.com/calendar.ics",
            binary.to_str().expect("utf-8 path"),
            directory.path().to_str().expect("utf-8 path"),
        ] {
            let app = TestApp::<CalendarShell>::launched(&[target]).expect(target);
            assert!(app.state().events.is_empty(), "{target} imported nothing");
            assert!(app.state().tasks.is_empty());
        }
    }

    /// Every scenario captures: the sample calendar around today, or a first
    /// run for the empty ones, with the view state each one names.
    #[test]
    fn every_scenario_stages_its_calendar_and_view() {
        let sample = crate::model::seed_demo(shot_now().date());
        for &name in SCENARIOS {
            let app = TestApp::<CalendarShell>::capture(Some(name)).expect(name);
            let state = app.state();
            if is_empty_scenario(Some(name)) {
                assert!(state.events.is_empty(), "{name} is a first run");
                continue;
            }
            let created = usize::from(matches!(
                name,
                "new_event" | "date_popover" | "time_popover" | "calendar_popover"
            ));
            assert_eq!(
                state.events.len(),
                sample.events.len() + created,
                "{name} holds the sample events"
            );
            assert_eq!(state.tasks.len(), sample.tasks.len(), "{name}");
            assert_eq!(state.projects.len(), sample.projects.len(), "{name}");
        }
        let design_review = CalendarShell::scenario(None);
        assert!(!design_review.is_empty());
        assert_eq!(
            CalendarShell::shot_viewport(Some("narrow_drawer")),
            (720, 900)
        );
    }

    /// The sample events' notes are their curated agendas.
    #[test]
    fn the_sample_calendar_opens_with_its_curated_notes() {
        let app = TestApp::<CalendarShell>::capture(Some("event_editor")).expect("capture");
        let review = app
            .state()
            .events
            .iter()
            .find(|event| event.title == "Design review")
            .expect("design review")
            .id;
        let notes = notes_of(&app, review);
        assert!(notes.contains("Agenda"), "{notes}");
        assert!(notes.contains("Confirm the launch timeline."), "{notes}");
    }

    #[test]
    fn painted_view_strip_swipes_invoke_live_navigation_and_preserve_document() {
        use winit::event::TouchPhase::{Cancelled, Ended, Started};
        fn control(
            node: &ui::headless::InspectionNode,
            target: &str,
        ) -> Option<ui::a11y::AccessNode> {
            fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<ui::a11y::AccessNode> {
                if node.action_target.as_deref() == Some(target) {
                    return Some(node.clone());
                }
                node.children.iter().find_map(|child| find(child, target))
            }
            node.semantics
                .as_ref()
                .and_then(|node| find(node, target))
                .or_else(|| {
                    node.children
                        .iter()
                        .find_map(|child| control(child, target))
                })
        }
        let mut app = TestApp::<CalendarShell>::new().unwrap();
        app.settle();
        let before = app.peer_document().get_heads();
        let start = control(&app.harness().inspection().root, "calendar.view:week")
            .unwrap()
            .bounds
            .unwrap()
            .to_rect()
            .center();
        app.harness().touch(1, Started, start.x, start.y);
        app.harness().touch(1, Ended, start.x - 90.0, start.y);
        app.settle();
        assert_eq!(
            control(&app.harness().inspection().root, "calendar.view:week")
                .unwrap()
                .state
                .checked,
            Some(true)
        );
        app.harness().touch(2, Started, start.x, start.y);
        app.harness().touch(2, Cancelled, start.x - 90.0, start.y);
        app.settle();
        assert_eq!(
            control(&app.harness().inspection().root, "calendar.view:week")
                .unwrap()
                .state
                .checked,
            Some(true)
        );
        app.harness().touch(3, Started, start.x, start.y);
        app.harness().touch(3, Ended, start.x + 90.0, start.y);
        app.settle();
        assert_eq!(
            control(&app.harness().inspection().root, "calendar.view:month")
                .unwrap()
                .state
                .checked,
            Some(true)
        );
        assert_eq!(app.peer_document().get_heads(), before);
        assert!(!app.undo(), "view changes create no document undo step");
    }

    /// Every edit the calendar offers on its items, and its calendars, reach
    /// the palette and agents through the one published tree, and invoking
    /// one edits the calendar.
    #[test]
    fn agents_reach_every_item_edit_and_the_calendars() {
        let mut app = TestApp::<CalendarShell>::capture(Some("month")).expect("capture");
        let surface = app.agent_surface().to_string();
        for needle in [
            "new-event",
            "new-task",
            "new-project",
            "set-start",
            "set-repeat",
            "set-reminder",
            "set-timezone",
            "invite",
            "move-to-calendar",
            "set-exception",
            "set-minutes-before",
            "respond",
            "make-organizer",
            "prepare-in-mail",
            "exclude",
            "set-due",
            "set-priority",
            "toggle",
            "set-color",
            "calendar.collection",
            "set-visibility",
        ] {
            assert!(surface.contains(needle), "missing {needle} in {surface}");
        }
        assert!(
            <CalendarEffect as ui::tilo_resource::Effect>::specs()
                .iter()
                .any(|spec| spec.id == "show-week"),
            "the views are the calendar's effects, offered beside its edits"
        );
        let event = app.state().events[0].id;
        app.call_on(
            &crate::actions::event_resource_id(event),
            "set-location",
            serde_json::json!({ "location": "Room 4" }),
        )
        .expect("set location");
        assert_eq!(app.state().event(event).expect("event").location, "Room 4");
    }

    /// Sync and export say why they cannot run: no account on this device,
    /// nothing to export.
    #[test]
    fn effects_that_cannot_run_are_refused_with_their_reason() {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        let sync = format!("{:?}", app.effect(&CalendarEffect::Sync).unwrap_err());
        assert!(sync.contains("account"), "{sync}");
        let export = format!("{:?}", app.effect(&CalendarEffect::ExportIcs).unwrap_err());
        assert!(export.contains("export"), "{export}");
    }

    /// Hiding a calendar or choosing its colour is this person's setting:
    /// kept beside the document, following them to their other devices.
    #[test]
    fn calendar_display_choices_are_personal_settings() {
        let mut app = TestApp::<CalendarShell>::capture(Some("month")).expect("capture");
        let (calendar, name) = crate::AccountStore::sample()
            .all_calendars()
            .find(|(_, calendar)| calendar.visible)
            .map(|(_, calendar)| (calendar.id.clone(), calendar.name.clone()))
            .expect("a shown sample calendar");
        let checkbox = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.role() == ui::accesskit::Role::CheckBox
                    && node
                        .label()
                        .is_some_and(|label| label.contains(name.as_str()))
            })
            .map(|(id, _)| id)
            .expect("the calendar's visibility checkbox");
        app.harness()
            .perform_a11y_action(checkbox, &ui::a11y::Action::ToggleCheck)
            .expect("toggle the checkbox");
        let display = app.settings::<crate::display::CalendarDisplay>().get();
        assert_eq!(display.visible.get(&calendar), Some(&false));
        assert!(
            !app.state().events.is_empty(),
            "hiding changes no calendar content"
        );
    }
}

#[cfg(test)]
mod person_account_tests {
    use super::*;
    use accounts::person::{ACCOUNTS_OWNER, PersonAccounts};
    use ui::framework::testing::TestApp;

    /// Settings, which keeps the person's accounts.
    struct AccountsOwner;

    impl ResourceApp for AccountsOwner {
        type Resource = Calendar;
        const ID: &'static str = ACCOUNTS_OWNER;
        const TITLE: &'static str = "Settings";

        fn view(_cx: &mut ViewCx<Calendar>) -> Element {
            Box::new(ui::widget::Column::new())
        }
    }

    fn person() -> String {
        "ca".repeat(32)
    }

    /// A `CalDAV` account Settings saved on one device reaches Calendar's
    /// window through the person's accounts record, and Calendar cannot
    /// change it.
    #[test]
    fn the_persons_caldav_accounts_reach_the_calendar_from_their_record() {
        let mut settings = TestApp::<AccountsOwner>::for_person(&person()).unwrap();
        let mut calendar = TestApp::<CalendarShell>::for_person(&person()).unwrap();
        let kept = settings.person::<PersonAccounts>().unwrap();
        let mut account = crate::account::CalendarAccount::new(crate::account::CalDavConfig {
            display_name: "Work calendars".into(),
            username: "work-dav".into(),
            ..crate::account::CalDavConfig::default()
        });
        account.config.password = "kept-on-the-device".into();
        let stored = account.to_stored_for_test();
        kept.invoke(&PersonAccounts::saving(&stored)).unwrap();

        let read = calendar
            .shared_person::<PersonAccounts>()
            .expect("Calendar reads the person's accounts");
        assert!(!read.editable());
        read.record()
            .merge_peer_document(kept.record().document().unwrap())
            .unwrap();
        calendar.settle();
        let shown = calendar
            .with_widget(|app: &mut crate::app::CalendarApp| {
                app.configured_for_test()
                    .accounts
                    .iter()
                    .map(|account| account.label().to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(shown, ["Work calendars"]);
        assert!(
            !String::from_utf8_lossy(&kept.record().document().unwrap())
                .contains("kept-on-the-device"),
            "the password never enters the record"
        );
    }
}

#[cfg(test)]
mod tasks_screen_tests {
    use super::*;
    use tilo_actions::{OperationKind, RetryPolicy};
    use ui::accesskit::{Node, NodeId, Role, Toggled};
    use ui::framework::testing::TestApp;

    /// Semantic effects run only in live mode; captures deliberately withhold
    /// effect runners. Seed the live test record through its published door.
    fn sample_calendar() -> TestApp<CalendarShell> {
        let mut app = TestApp::<CalendarShell>::new().expect("live test app");
        let model = crate::model::seed_demo(chrono::Local::now().date_naive());
        let actions = sample_actions(&model)
            .iter()
            .map(|action| Invoke::apply(action).expect("sample action"))
            .collect::<Vec<_>>();
        app.invoke(Invoke::group(actions))
            .expect("seed through the calendar door");
        app.call("show-month", serde_json::json!({}))
            .expect("Month view");
        app
    }

    /// The accessible "Task list" region and the names of its items, when
    /// the Tasks screen is on screen.
    fn task_list(app: &mut TestApp<CalendarShell>) -> Option<Vec<String>> {
        let nodes = app.harness().a11y_nodes();
        let (_, list) = nodes
            .iter()
            .find(|(_, node)| node.role() == Role::List && node.label() == Some("Task list"))?;
        let items = list
            .children()
            .iter()
            .filter_map(|child| nodes.iter().find(|(id, _)| id == child))
            .filter_map(|(_, node)| node.label().map(str::to_owned))
            .collect();
        Some(items)
    }

    /// The accessible node whose name is exactly `name`.
    fn named(app: &mut TestApp<CalendarShell>, name: &str) -> Option<(NodeId, Node)> {
        app.harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some(name))
    }

    fn activate(app: &mut TestApp<CalendarShell>, name: &str) {
        let (id, _) = named(app, name).unwrap_or_else(|| panic!("no control named {name}"));
        app.harness()
            .perform_a11y_action(id, &ui::a11y::Action::Activate)
            .unwrap_or_else(|error| panic!("activate {name}: {error:?}"));
        app.settle();
    }

    /// `show-tasks` is the root's typed view action, offered while the
    /// calendar is closed so a launcher card reaches the tasks in one step.
    #[test]
    fn show_tasks_is_a_launchable_idempotent_view_on_the_root() {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        let resources = app.harness().resources();
        let root = resources
            .iter()
            .find(|resource| resource.id.as_str() == Calendar::KIND)
            .expect("the calendar root is published");
        let show = root
            .actions
            .iter()
            .find(|action| action.id == "show-tasks")
            .expect("show-tasks is published on the root");
        assert_eq!(show.label, "Show tasks");
        assert_eq!(show.kind, OperationKind::View);
        assert_eq!(
            show.contract.permission,
            tilo_actions::PermissionRequirement::Read
        );
        assert_eq!(show.contract.retry, RetryPolicy::Idempotent);
        assert_eq!(
            show.contract.discovery,
            tilo_actions::DiscoveryScope::Launchable
        );
        assert!(show.contract.validate_declaration().is_empty());
        let view = root
            .actions
            .iter()
            .find(|action| action.id == "show-view")
            .expect("show-view");
        assert!(
            view.params().iter().any(|param| param
                .choices
                .iter()
                .any(|choice| AsRef::<str>::as_ref(choice) == "tasks")),
            "show-view offers the Tasks screen"
        );
    }

    #[test]
    fn viewers_can_show_tasks_but_cannot_add_tasks() {
        let mut app =
            TestApp::<CalendarShell>::with_role(ui::permissions::Role::Viewer).expect("viewer app");
        let before = app.state().tasks;
        app.call("show-tasks", serde_json::json!({}))
            .expect("viewers can change their view");
        assert_eq!(task_list(&mut app), Some(Vec::new()));
        assert!(
            before.is_empty() && app.state().tasks.is_empty(),
            "showing Tasks writes no task state"
        );
        let failure = app.call("new-task", serde_json::json!({ "title": "Refused task" }));
        assert!(
            failure.is_err(),
            "view permission never admits a task mutation"
        );
        assert!(app.state().tasks.is_empty());
    }

    /// Invoked through the published door, `show-tasks` lands on the Tasks
    /// screen over the Inbox, whichever list the sidebar had chosen; the
    /// "Task list" region exists only there.
    #[test]
    fn show_tasks_lands_on_the_inbox_task_list() {
        let mut app = sample_calendar();
        assert!(task_list(&mut app).is_none(), "Month shows no task list");
        assert!(
            named(&mut app, "Tasks view").is_some(),
            "the bar's Tasks tab is not the task list"
        );
        activate(&mut app, "Show tasks in Today");

        app.call("show-tasks", serde_json::json!({}))
            .expect("show-tasks");
        let items = task_list(&mut app).unwrap_or_else(|| {
            panic!(
                "the Tasks screen publishes its list: {:?}",
                app.harness()
                    .a11y_nodes()
                    .iter()
                    .filter_map(|(_, n)| n.label())
                    .collect::<Vec<_>>()
            )
        });
        let inbox: Vec<_> = app
            .state()
            .tasks_for(
                &crate::model::ListSel::Inbox,
                chrono::Local::now().date_naive(),
            )
            .iter()
            .map(|task| task.title.clone())
            .collect();
        assert!(!inbox.is_empty(), "the sample Inbox holds a task");
        assert_eq!(items.len(), inbox.len(), "{items:?}");
        for (item, title) in items.iter().zip(&inbox) {
            assert!(item.starts_with(title.as_str()), "{item} names {title}");
        }
        let (_, tab) = named(&mut app, "Tasks view").expect("the Tasks tab");
        assert_eq!(
            tab.toggled(),
            Some(Toggled::True),
            "the Tasks tab is chosen"
        );

        for view in ["month", "week", "day", "agenda"] {
            app.call("show-view", serde_json::json!({ "view": view }))
                .expect(view);
            assert!(task_list(&mut app).is_none(), "{view} shows no task list");
        }
        app.call("show-view", serde_json::json!({ "view": "tasks" }))
            .expect("show-view tasks");
        assert!(task_list(&mut app).is_some());
    }

    /// The bar's Tasks tab and the action reach the same screen: choosing
    /// the Inbox and the tab shows what `show-tasks` shows.
    #[test]
    fn the_tasks_tab_and_show_tasks_reach_the_same_list() {
        let mut by_tab = sample_calendar();
        activate(&mut by_tab, "Show tasks in Inbox");
        activate(&mut by_tab, "Tasks view");
        let mut by_action = sample_calendar();
        by_action
            .call("show-tasks", serde_json::json!({}))
            .expect("show-tasks");
        let tab = task_list(&mut by_tab).expect("the tab shows the task list");
        assert_eq!(Some(tab), task_list(&mut by_action));
    }

    /// An empty list says so, and the entry field adds a task to it through
    /// the same edit `new-task` makes.
    #[test]
    fn an_empty_task_list_explains_itself_and_its_field_adds_a_task() {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        app.call("show-tasks", serde_json::json!({}))
            .expect("show-tasks");
        assert_eq!(task_list(&mut app), Some(Vec::new()));
        assert!(
            named(
                &mut app,
                "Inbox is empty. Type a task in the field above and press Enter."
            )
            .is_some(),
            "the empty state is announced"
        );

        activate(&mut app, "Add a task to Inbox");
        app.harness().type_text("Water the plants");
        app.harness().key("Enter");
        app.settle();
        let tasks = app.state().tasks;
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!(task.title, "Water the plants");
        assert!(!task.done && task.project.is_none() && task.parent.is_none());
        assert_eq!(
            task_list(&mut app),
            Some(vec!["Water the plants".to_owned()])
        );
        assert!(app.undo(), "adding a task is one undo step");
        assert!(app.state().tasks.is_empty());
    }
}
