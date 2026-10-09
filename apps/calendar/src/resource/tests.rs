use super::*;
use crate::model::{Id, key};
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Replica, scope_contains};

fn notes_of(app: &TestApp<CalendarShell>, item: Id) -> String {
    app.read_document(|doc| read_doc(doc).notes_text(item))
}

fn state_of(app: &TestApp<CalendarShell>) -> Calendar {
    app.read_document(read_doc)
}

/// A new event on the local calendar, as a verb takes it.
fn new_event(title: &str, start: &str, end: &str) -> Invoke<Calendar> {
    Calendar::NEW_EVENT.with(calendar::NewEvent {
        event: EventData {
            id: Uuid::new_v4().to_string(),
            title: title.into(),
            start: start.into(),
            end: end.into(),
            ..EventData::default()
        },
    })
}

/// Every entry lies in the events' or the tasks' scope, and a project's
/// tasks also in that project's scope.
#[test]
fn entries_nest_under_the_declared_scopes() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 6, 30).expect("date");
    let model = crate::model::seed_demo(today);
    let scopes = model.scopes();
    let entries = model.search_entries();
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

/// A search hit opens the item it names; one whose item is gone is refused
/// with the reason.
#[test]
fn a_search_hit_opens_its_item_and_a_gone_one_is_refused() {
    let mut app = TestApp::<CalendarShell>::capture(Some("month")).expect("capture");
    let review = state_of(&app)
        .events
        .iter()
        .find(|event| event.title == "Design review")
        .expect("design review")
        .id;
    let hit = state_of(&app)
        .search_entries()
        .into_iter()
        .find(|entry| entry.title == "Design review")
        .expect("the event is found");
    assert_eq!(
        hit.anchor.as_deref(),
        Some(crate::actions::event_resource_id(review).as_str())
    );
    app.call("open", serde_json::json!({ "anchor": hit.anchor }))
        .expect("the hit opens");
    app.settle();
    let opened = app
        .with_widget(|calendar: &mut CalendarApp| calendar.selected_event())
        .expect("the calendar");
    assert_eq!(opened, Some(review));

    let gone = crate::actions::event_resource_id(Uuid::new_v4());
    assert!(
        app.call("open", serde_json::json!({ "anchor": gone }))
            .is_err()
    );
}

/// Typing in an open event's notes edits them at their path, a word per
/// undo step, kept when the calendar is opened again.
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
    assert!(app.undo());
    assert_eq!(notes_of(&app, event), "Agenda ");
    assert!(app.redo());

    let app = app.reopen().expect("reopen");
    assert_eq!(notes_of(&app, event), "Agenda items");
}

/// The palette, agents and scripts edit an event's notes as the field
/// edit at their path, and what they do is undone like a person's edit.
#[test]
fn an_agent_edits_an_events_notes_at_their_path() {
    let mut app = TestApp::<CalendarShell>::new().expect("test app");
    let id = Uuid::new_v4();
    app.invoke(Calendar::NEW_EVENT.with(calendar::NewEvent {
        event: EventData {
            id: id.to_string(),
            title: "Review".into(),
            start: "2026-09-08T09:00".into(),
            end: "2026-09-08T10:00".into(),
            ..EventData::default()
        },
    }))
    .expect("create");
    assert!(
        app.actions()
            .expect("the calendar's actions")
            .iter()
            .any(|action| action.address.action == tilo_resource::FIELD_EDIT),
        "the calendar's fields, its notes among them, are field edits"
    );
    app.invoke(Invoke::from(
        notes_path(id)
            .perform(&RichTextAction::AppendParagraph {
                text: "Bring the mockups".into(),
            })
            .expect("a notes edit"),
    ))
    .expect("append");
    assert_eq!(notes_of(&app, id), "Bring the mockups");
    assert!(app.undo());
    assert_eq!(notes_of(&app, id), "");
}

/// Creating an event on a remote calendar and undoing it after its server
/// took the upload leaves the server's copy to delete: undo reaches the
/// server like any other change.
#[test]
fn undo_of_an_uploaded_remote_event_deletes_it_on_the_server() {
    let mut app = TestApp::<CalendarShell>::new().expect("test app");
    let accounts = crate::AccountStore::sample();
    let remote = accounts
        .all_calendars()
        .next()
        .map(|(_, calendar)| calendar.id.clone())
        .expect("a sample calendar");
    let id = Uuid::new_v4();
    let event = EventData {
        id: id.to_string(),
        title: "Standup".into(),
        start: "2026-09-08T09:00".into(),
        end: "2026-09-08T09:30".into(),
        calendar_id: remote.clone(),
        ..EventData::default()
    };
    app.invoke(Calendar::NEW_EVENT.with(calendar::NewEvent {
        event: event.clone(),
    }))
    .expect("create");
    let uploads = crate::outbox::uploads(&state_of(&app), &accounts);
    assert_eq!(uploads.len(), 1, "the new event waits for its upload");

    // The device holding the account uploaded it and recorded that.
    app.invoke(Calendar::SERVER_APPLIED.with(calendar::ServerApplied {
        event: id,
        at: RemoteAt {
            calendar: remote.clone(),
            href: format!("/dav/{id}.ics"),
            etag: Some("\"1\"".into()),
        },
        holds: fingerprint(&event.to_event().expect("an event")),
    }))
    .expect("record the upload");
    assert!(crate::outbox::uploads(&state_of(&app), &accounts).is_empty());

    assert!(app.undo(), "undo takes back the create, not the upload");
    assert!(state_of(&app).event(id).is_none());
    let deletes = crate::outbox::deletes(&state_of(&app), &accounts);
    assert_eq!(deletes.len(), 1, "the server's copy is deleted next");
    assert_eq!(deletes[0].0, id);
    assert_eq!(deletes[0].1.calendar.id, remote);
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
    fn control(node: &ui::headless::InspectionNode, target: &str) -> Option<ui::a11y::AccessNode> {
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

/// Every verb on the calendar's items, its field edits and its calendars
/// reach the palette and agents through the one published tree, and
/// invoking one edits the calendar.
#[test]
fn agents_reach_every_item_verb_and_the_calendars() {
    let mut app = TestApp::<CalendarShell>::capture(Some("month")).expect("capture");
    let surface = app.agent_surface().to_string();
    for needle in [
        "new-event",
        "new-task",
        "new-project",
        "delete-event",
        "reschedule",
        "move-to-calendar",
        "invite",
        "delete-task",
        "delete-project",
        "import-items",
        "field-edit",
        "prepare-in-mail",
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
    let event = app.state().events[0].clone();
    app.call_on(
        &crate::actions::event_resource_id(event.id),
        "reschedule",
        serde_json::json!({ "start": "2026-07-01T09:00:00", "end": "2026-07-01T10:30:00" }),
    )
    .expect("reschedule");
    let moved = app.state().event(event.id).expect("event").clone();
    assert_eq!(
        moved.start,
        at(chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(), 9, 0)
    );
    assert_eq!(
        moved.end,
        at(chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(), 10, 30)
    );
    app.call_on(
        &crate::actions::event_resource_id(event.id),
        "invite",
        serde_json::json!({ "email": "sam@example.com", "name": "Sam" }),
    )
    .expect("invite");
    assert!(
        app.state()
            .event(event.id)
            .expect("event")
            .attendees
            .iter()
            .any(|attendee| attendee.email == "sam@example.com")
    );
    assert!(
        app.call_on(
            &crate::actions::event_resource_id(event.id),
            "reschedule",
            serde_json::json!({ "start": "2026-07-01T09:00:00", "end": "2026-07-01T08:00:00" }),
        )
        .is_err(),
        "an event does not end before it starts"
    );
}

/// What is more than setting a field: a task's steps go with it, a deleted
/// project's tasks stay unfiled, and an event's notes go with the event.
#[test]
fn verbs_take_along_what_hangs_off_an_item() {
    let mut calendar = Replica::<Calendar>::new();
    calendar
        .apply(Calendar::NEW_PROJECT.with(calendar::NewProject {
            name: "Work".into(),
        }))
        .unwrap();
    let project = calendar.state().unwrap().projects[0].id;
    let parent = Uuid::new_v4();
    let step = Uuid::new_v4();
    for (id, of) in [(parent, None), (step, Some(parent))] {
        calendar
            .apply(Calendar::NEW_TASK.with(calendar::NewTask {
                task: TaskData {
                    id: id.to_string(),
                    title: "Ship".into(),
                    project: project.to_string(),
                    parent: of.map(|of| of.to_string()).unwrap_or_default(),
                    ..TaskData::default()
                },
            }))
            .unwrap();
    }
    assert!(
        calendar
            .apply(Calendar::NEW_TASK.with(calendar::NewTask {
                task: TaskData {
                    title: "   ".into(),
                    ..TaskData::default()
                },
            }))
            .is_err(),
        "a task needs a title"
    );
    calendar
        .apply_confirmed(Calendar::DELETE_PROJECT.with(key(project)))
        .unwrap();
    let state = calendar.state().unwrap();
    assert!(state.projects.is_empty());
    assert!(
        state.tasks.iter().all(|task| task.project.is_none()),
        "its tasks stay, unfiled"
    );
    assert_eq!(state.tasks.len(), 2);
    calendar
        .apply(Calendar::DELETE_TASK.with(key(parent)))
        .unwrap();
    let state = calendar.state().unwrap();
    assert!(state.tasks.is_empty(), "the task went with its step");
    assert!(!state.notes.contains_key(&step.to_string()));

    calendar
        .apply(new_event("Lunch", "2026-09-08T12:00", "2026-09-08T13:00"))
        .unwrap();
    let event = calendar.state().unwrap().events[0].id;
    assert!(
        calendar
            .state()
            .unwrap()
            .notes
            .contains_key(&event.to_string())
    );
    calendar
        .apply(Calendar::DELETE_EVENT.with(key(event)))
        .unwrap();
    let state = calendar.state().unwrap();
    assert!(state.events.is_empty());
    assert!(state.notes.is_empty(), "the notes went with the event");
    assert!(
        calendar
            .apply(Calendar::DELETE_EVENT.with(key(event)))
            .is_err(),
        "a gone event is refused"
    );
}

/// What a field can hold is checked whoever edits it: an event's end, its
/// reminder and organizer; a project's name.
#[test]
fn field_edits_are_checked_whoever_makes_them() {
    let mut calendar = Replica::<Calendar>::new();
    calendar
        .apply(new_event("Lunch", "2026-09-08T12:00", "2026-09-08T13:00"))
        .unwrap();
    let event = calendar.state().unwrap().events[0].clone();
    let fields = Calendar::paths().events.item(&key(event.id));
    for (edit, why) in [
        (
            fields.end.set(at(
                chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
                11,
                0,
            )),
            "an end before the start",
        ),
        (
            fields.reminder_min.set(Some(7)),
            "a reminder it does not offer",
        ),
        (fields.recurrence_interval.set(0), "repeating every 0"),
        (
            fields.timezone.set("Mars/Olympus".into()),
            "an unknown time zone",
        ),
        (
            fields.organizer.set(Some(Uuid::new_v4())),
            "an organizer who is no attendee",
        ),
    ] {
        assert!(
            calendar.apply(Invoke::from(edit)).is_err(),
            "{why} is refused"
        );
    }
    assert_eq!(
        calendar.state().unwrap().events[0],
        event,
        "nothing changed"
    );
    calendar
        .apply(Invoke::from(fields.reminder_min.set(Some(30))))
        .unwrap();
    assert_eq!(calendar.state().unwrap().events[0].reminder_min, Some(30));
}

/// Two people changing different fields of one event at once both keep
/// their change.
#[test]
fn concurrent_field_edits_of_one_event_both_land() {
    let mut left = Replica::<Calendar>::new();
    left.apply(new_event("Lunch", "2026-09-08T12:00", "2026-09-08T13:00"))
        .unwrap();
    let mut right = left.fork();
    let event = left.state().unwrap().events[0].id;
    let fields = Calendar::paths().events.item(&key(event));
    left.apply(Invoke::from(fields.title.set("Team lunch".into())))
        .unwrap();
    right
        .apply(Invoke::from(fields.location.set("Café Mori".into())))
        .unwrap();
    left.sync_with(&mut right).unwrap();
    for replica in [&left, &right] {
        let state = replica.state().unwrap();
        assert_eq!(state.events[0].title, "Team lunch");
        assert_eq!(state.events[0].location, "Café Mori");
    }
}

/// What an agent does for a person reads back in the reader's language as
/// the calendar's sentences.
#[test]
fn history_entries_render_as_the_calendars_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<CalendarShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    app.call_by(
        agent.clone(),
        "calendar.calendar",
        "new-event",
        serde_json::json!({ "event": {
            "title": "Lunch", "start": "2026-09-08T12:00", "end": "2026-09-08T13:00"
        } }),
    )
    .unwrap();
    let event = app.state().events[0].id;
    let fields = Calendar::paths().events.item(&key(event));
    app.call_by(
        agent.clone(),
        "calendar.calendar",
        "field-edit",
        serde_json::json!({ "edit": fields.title.set("Team lunch".into()) }),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        &crate::actions::event_resource_id(event),
        "reschedule",
        serde_json::json!({ "start": "2026-09-08T12:30:00", "end": "2026-09-08T13:30:00" }),
    )
    .unwrap();
    app.call_by(
        agent,
        &crate::actions::event_resource_id(event),
        "delete-event",
        serde_json::json!({}),
    )
    .unwrap();

    let english = LOCALES
        .iter()
        .find(|(locale, _)| *locale == "en-US")
        .map(|(_, source)| *source)
        .unwrap();
    let catalogue = tilo_history::Catalogue::new("en-US", &[english]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .map(|entry| reader.sentence(&entry))
        .collect();
    assert_eq!(
        sentences,
        [
            "Added an event",
            "Renamed an event to Team lunch",
            "Moved an event",
            "Deleted an event",
        ]
    );
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

mod person_accounts {
    use super::*;
    use accounts::person::{ACCOUNTS_OWNER, PersonAccounts};

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
