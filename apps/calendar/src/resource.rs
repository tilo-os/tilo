//! The calendar's window, the effects it offers and the app the launcher
//! opens. Its resources and verbs are the model's ([`calendar_model`]); what
//! draws them, and what a capture shows, is this crate's.

use tilo_document::{RichText, RichTextAction};
use ui::framework::{Dispatcher, Fixture, LaunchInput, ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, Effect, Invoke, Refusal};
use ui::widget::Element;
use uuid::Uuid;

use calendar_model::actions::{EventData, TaskData};
use calendar_model::model::Calendar;
#[cfg(test)]
use calendar_model::model::Event;
pub(crate) use calendar_model::resource::{CalendarAction, notes_path, notes_resource_id};
#[cfg(test)]
use jiff::civil::DateTime;
#[cfg(test)]
use ui::tilo_resource::Doc;

pub use calendar_model::actions::{import, import_reporting, imported_status};
pub use calendar_model::calendar;
pub use calendar_model::resource::{EVENTS_SCOPE, TASKS_SCOPE, project_scope};
pub use calendar_model::resource::{RemoteAt, fingerprint};

use crate::app::CalendarApp;

ui::embedded_catalog!(pub(crate) fn, ["de-DE", "en-US"]);

pub(crate) fn notes_dispatcher(
    calendar: &Dispatcher<Calendar>,
    id: calendar_model::model::Id,
) -> Dispatcher<RichText> {
    let notes = notes_path(id);
    calendar.child::<RichText>(&notes_resource_id(&id), move |op| tilo_resource::Stamped {
        operation: CalendarAction::FieldEdit(notes.edit(tilo_resource::EditOp::Rich {
            rich: serde_json::to_value(&op).unwrap_or_default(),
        })),
        now_ms: 0,
    })
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "calendar.effects")]
pub enum CalendarEffect {
    /// Sync calendars
    ///
    /// Pulls the calendars of this device's accounts from their servers.
    #[effect(kind = "outside")]
    #[effect(job)]
    Sync,
    /// Export ICS
    ///
    /// Saves every event and task to an iCalendar file you choose.
    #[effect(kind = "outside")]
    #[effect(attended, job)]
    ExportIcs,
    /// Export ICS content
    ///
    /// Keeps every event and task, or one event, as an iCalendar file in
    /// the workspace's content and answers its content address. No file is
    /// chosen or written, so it needs no one attending: the file stays in
    /// the workspace until someone hands it on.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    ExportIcsContent {
        #[param("The event's id; every event and task when omitted")]
        event: Option<Uuid>,
    },
    /// Import ICS
    ///
    /// Imports the events and tasks of an iCalendar file you choose.
    #[effect(kind = "outside")]
    #[effect(attended, job)]
    ImportIcs,
    /// Invite from Contacts
    ///
    /// Shows the people of your address book to add to an event; each one
    /// is added only once you confirm.
    #[effect(view)]
    ContactAttendees {
        #[param("Event id")]
        event_id: String,
    },
    /// Prepare invitation in Mail
    ///
    /// Answers with the arguments of Mail's invitation draft for one attendee;
    /// nothing is sent.
    #[effect(kind = "outside", on = crate::rows::AttendeeRow)]
    #[effect(returns, audience = "agents")]
    PrepareInMail {
        #[param("The attendee row's id")]
        attendee: String,
    },
    /// Set a calendar's colour
    ///
    /// A display choice of this person, which follows them to their other
    /// devices.
    #[effect(kind = "outside", on = crate::rows::CalendarRow)]
    SetColor {
        #[param("Calendar id")]
        id: String,
        #[param("indigo, emerald, amber, red, blue, violet or slate")]
        color: String,
    },
    /// Read calendar
    ///
    /// Answers what one calendar is: its name, whether its events are
    /// shown, its colour and how many events it holds. It is the calendar
    /// row's read: what discloses the row, so showing, hiding and
    /// recolouring it are offered to whoever may read it, an agent too.
    #[effect(
        view,
        row,
        returns,
        idempotent,
        on = crate::rows::CalendarRow,
        tag = tilo_actions::DOCUMENT_READ_TAG
    )]
    ReadCalendar {
        #[param("Calendar id")]
        id: String,
    },
    /// Show calendar
    ///
    /// Shows one calendar's events.
    #[effect(kind = "outside", on = crate::rows::CalendarRow)]
    ShowCalendar {
        #[param("Calendar id")]
        id: String,
    },
    /// Hide calendar
    ///
    /// Hides one calendar's events.
    #[effect(kind = "outside", on = crate::rows::CalendarRow)]
    HideCalendar {
        #[param("Calendar id")]
        id: String,
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
    #[effect(view, idempotent)]
    ShowTasks,
    /// New event
    ///
    /// Starts an event the way the window's own New event does: at the next
    /// quarter hour on the day the window shows, its form open with the caret
    /// in the title.
    #[effect(
        view,
        idempotent,
        icon = "plus",
        keywords = "create, add, schedule, start"
    )]
    StartEvent,
    /// New task
    ///
    /// Opens the task list's entry, ready for the new task's name.
    #[effect(view, idempotent, icon = "plus", keywords = "create, add, todo, start")]
    StartTask,
    /// Open
    ///
    /// Shows an event, one occurrence of it, a task, a project's tasks or
    /// one calendar's events.
    #[effect(view, row, idempotent, on = crate::rows::CalendarRow)]
    Open {
        #[param("The event, occurrence, task, project or calendar's resource id")]
        item: String,
    },
}

pub struct CalendarShell;

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
    "quick_add",
    "shortcuts",
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

fn is_empty_scenario(scenario: Option<&str>) -> bool {
    scenario.is_some_and(|name| name.starts_with("empty"))
}

/// What a capture shows in place of the person's `CalDAV` accounts: sample accounts whose servers
/// answer from bundled sample calendars, never the network.
pub(crate) struct SampleAccounts(pub(crate) crate::CalendarAccounts);

/// The moment a capture shows as now: the same day and time on every run,
/// so the sample calendar, today's highlight and the now line never move.
pub(crate) struct ShotNow(pub(crate) jiff::civil::DateTime);

pub(crate) const SHOT_ZONE: &str = "Europe/Berlin";

pub(crate) fn shot_now() -> jiff::civil::DateTime {
    jiff::civil::date(2026, 6, 30).at(10, 30, 0, 0)
}

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

pub(crate) fn sample_invocations(sample: &Calendar) -> Vec<Invoke<Calendar>> {
    let paths = Calendar::paths();
    let projects = sample
        .projects
        .iter()
        .map(|project| Invoke::from(paths.projects.insert(project)));
    let events = sample.events.iter().map(|event| {
        Calendar::NEW_EVENT.with(calendar::NewEvent {
            event: EventData::from_event(event),
        })
    });
    let depth = |task: &calendar_model::model::Task| {
        std::iter::successors(task.parent, |parent| sample.task(*parent)?.parent)
            .take(sample.tasks.len())
            .count()
    };
    let mut ordered: Vec<_> = sample.tasks.iter().collect();
    ordered.sort_by_key(|task| depth(task));
    let tasks = ordered.into_iter().map(|task| {
        Calendar::NEW_TASK.with(calendar::NewTask {
            task: TaskData::from_task(task),
        })
    });
    let notes = SAMPLE_NOTES.iter().filter_map(|(title, markdown)| {
        let event = sample
            .events
            .iter()
            .find(|event| event.title.starts_with(title))?;
        notes_path(event.id)
            .perform(&RichTextAction::ReplaceBody {
                markdown: (*markdown).to_owned(),
            })
            .ok()
            .map(Invoke::from)
    });
    projects.chain(events).chain(tasks).chain(notes).collect()
}

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

const COMMANDS: &[ui::framework::Command] = &[
    ui::framework::Command {
        option: "--probe-timezone-engine",
        run: probe_timezone_engine,
    },
    ui::framework::Command {
        option: "--probe-contact-directory-engine",
        run: probe_contact_directory_engine,
    },
];

#[cfg(feature = "runtime-engine")]
fn probe_timezone_engine(_: &str) -> Result<(), String> {
    let report = tilo_timezone_engine::self_check()?;
    println!("calendar --probe-timezone-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_timezone_engine(_: &str) -> Result<(), String> {
    Err("the time-zone engine library needs the `runtime-engine` feature".to_string())
}

#[cfg(feature = "runtime-engine")]
fn probe_contact_directory_engine(_: &str) -> Result<(), String> {
    let report = tilo_contact_directory_engine::self_check()?;
    println!("calendar --probe-contact-directory-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_contact_directory_engine(_: &str) -> Result<(), String> {
    Err("the contact directory's engine library needs the `runtime-engine` feature".to_string())
}

impl ResourceApp for CalendarShell {
    type Resource = Calendar;
    const ID: &'static str = ui::apps::CALENDAR;
    const TITLE: &'static str = "Calendar";
    const TITLE_KEY: &'static str = "calendar-app-title";
    const ICON: Option<&'static str> = Some("calendar");
    const OPENS: &'static [&'static str] = &["text/calendar"];
    const ARGUMENTS: usize = 1;

    const PERSON_RESOURCES: &'static [ui::framework::PersonKind] = &[
        ui::framework::PersonKind::shared::<accounts::person::AccountList>(),
        ui::framework::PersonKind::shared::<tilo_contact_directory::ContactDirectory>(),
    ];

    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }

    fn view(cx: &mut ViewCx<Calendar>) -> Element {
        let app = CalendarApp::new();
        let store = app.shared();
        let live = cx.live().clone();
        cx.on_open_item(move |item| {
            let resource = live.get().item_resource(item).ok_or_else(|| {
                Refusal::new(format!(
                    "`{item}` is no event, task or project of this calendar"
                ))
            })?;
            store.borrow_mut().ask(crate::app::Ask::OpenItem(resource));
            Ok(())
        });
        let places = ui::widget::NavFeed::new();
        let month = ui::widget::DateFeed::new();
        let shell = ui::widget::NavShell::new()
            .with_id("calendar-shell")
            .content_kind(ui::widget::ContentKind::Document)
            .with_window_toolbar();
        let app = app.in_shell(&places, &month, shell.window_bar_link());
        Box::new(
            shell
                .sidebar(crate::app::sidebar(&places, &month))
                .content(crate::app::window(cx, app)),
        )
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn import_resource(context: &ui::application::ProcessContext) -> Result<(), String> {
        crate::import_inbox::import(context)
    }

    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Calendar>> {
        let source = match launched_ics(input) {
            Ok(Some(source)) => source,
            Ok(None) => return Vec::new(),
            Err(error) => {
                tracing::warn!(%error, "calendar: the launch file was not imported");
                return Vec::new();
            }
        };
        let import = import(&source);
        if import.is_none() {
            tracing::warn!("calendar: the launch file holds no supported calendar items");
        }
        import.into_iter().collect()
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
            fixture.provide(SampleAccounts(crate::CalendarAccounts::sample()));
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Calendar>> {
        if is_empty_scenario(name) {
            return Vec::new();
        }
        sample_invocations(&calendar_model::model::seed_demo(shot_now().date()))
    }
}

#[cfg(test)]
pub(crate) fn read_doc(doc: &Doc) -> Calendar {
    Calendar::read_whole(doc).expect("a calendar document")
}

#[cfg(test)]
pub(crate) fn event_at(title: &str, start: DateTime) -> Event {
    Event {
        id: Uuid::new_v4(),
        title: title.to_owned(),
        start,
        end: start + jiff::SignedDuration::from_hours(1),
        recurrence_interval: 1,
        timezone: "floating".into(),
        ..Event::default()
    }
}

#[cfg(test)]
pub(crate) fn at(date: jiff::civil::Date, hour: i8, minute: i8) -> DateTime {
    date.at(hour, minute, 0, 0)
}

#[cfg(test)]
mod tests;

// Each capture scenario publishes its own set of actions, and driving every
// one through the window takes a while: four tests ask a quarter of the
// scenarios each, running beside one another.
ui::reach_tests!(
    CalendarShell,
    shares: [share_0, share_1, share_2, share_3]
);

/// German is one of the languages the app ships, and it reads in German: the
/// copy comes from `locales/de-DE.ftl`, never from the English source
/// catalogue beside it.
#[cfg(test)]
mod german_window {
    #[test]
    fn a_german_window_reads_the_catalogue() {
        let catalog = &super::CATALOG;
        assert!(
            catalog.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            catalog.languages()
        );
        catalog.set_locale("de-DE");
        assert_eq!(catalog.t("calendar-view-month"), "Monat");
        catalog.set_locale("en-US");
        assert_eq!(catalog.t("calendar-view-month"), "Month");
    }
}
