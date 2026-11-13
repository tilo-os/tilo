use super::*;
use crate::model::{Id, key};
use accounts::person::PersonAccounts;
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Replica, scope_contains};

#[test]
fn read_only_upload_refusal_keeps_the_event_and_queued_outbox_in_the_source_record() {
    let server = test_http::Responder::silent();
    let person = "cb".repeat(32);
    let mut settings = TestApp::<person_accounts::AccountsOwner>::for_person(&person).unwrap();
    let mut app = TestApp::<CalendarShell>::for_person(&person).expect("test app");
    let kept = settings.shared_person::<PersonAccounts>().unwrap();
    let mut account = crate::AccountStore::sample().accounts.remove(0);
    account.config.server_url = server.url();
    account.calendars.truncate(1);
    account.calendars[0].read_only = true;
    let remote = account.calendars[0].id.clone();
    kept.invoke(&PersonAccounts::saving(&account.to_stored_for_test()))
        .unwrap();
    let read = app.shared_person::<PersonAccounts>().unwrap();
    read.record()
        .merge_peer_document(kept.record().document().unwrap())
        .unwrap();
    app.settle();
    let id = Uuid::new_v4();
    app.invoke(Calendar::NEW_EVENT.with(calendar::NewEvent {
        event: EventData {
            id: id.to_string(),
            title: "Keep my local edit".into(),
            start: "2026-09-08T09:00".into(),
            end: "2026-09-08T09:30".into(),
            calendar_id: remote,
            ..EventData::default()
        },
    }))
    .expect("create in the source record");
    let configured = app
        .with_widget(|surface: &mut CalendarApp| surface.configured_for_test().clone())
        .unwrap();
    let jobs: Vec<_> = crate::outbox::uploads(&state_of(&app), &configured)
        .into_iter()
        .map(|(_, job)| job)
        .collect();
    assert_eq!(jobs.len(), 1);
    let result = ui::rt::block_on(crate::caldav::push_jobs(&jobs)).unwrap();
    assert_eq!(result.failures, [(id, "Calendar is read-only".into())]);
    assert!(result.confirmed.is_empty());
    app.settle();
    let state = state_of(&app);
    assert_eq!(state.uploads.pending().count(), 1);
    assert_eq!(state.uploads.entries().len(), 1);
    assert_eq!(
        state.event(id).expect("local edit survives").title,
        "Keep my local edit"
    );
    assert!(
        !state.remote.contains_key(&id.to_string()),
        "a refusal cannot claim server confirmation"
    );
    server.assert_untouched();
}

fn notes_of(app: &TestApp<CalendarShell>, item: Id) -> String {
    app.read_document(|doc| read_doc(doc).notes_text(item))
}

fn state_of(app: &TestApp<CalendarShell>) -> Calendar {
    app.read_document(read_doc)
}

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

    let task = state_of(&app).tasks[0].id;
    app.call_on(
        &crate::actions::task_resource_id(task),
        "open",
        serde_json::json!({}),
    )
    .expect("the task opens");
    app.settle();
    let opened = app
        .with_widget(|calendar: &mut CalendarApp| calendar.selected_task())
        .expect("the calendar");
    assert_eq!(opened, Some(task));
}

#[test]
fn typing_in_an_events_notes_edits_them_through_the_calendar() {
    let mut app = TestApp::<CalendarShell>::new().expect("test app");
    app.harness().key("n");
    let event = app.state().events.first().expect("n creates an event").id;
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

fn bar_view() -> Option<String> {
    let views = ui::window_toolbar::declared().and_then(|toolbar| toolbar.views)?;
    views
        .choices
        .into_iter()
        .find(|choice| choice.id == views.chosen)
        .map(|choice| choice.label)
}

fn choose_view_in_bar(app: &mut TestApp<CalendarShell>, view: crate::model::View) {
    let name = view.display_label();
    let slot = ui::window_toolbar::declared()
        .and_then(|toolbar| toolbar.views)
        .and_then(|views| {
            views
                .choices
                .into_iter()
                .find(|choice| choice.label == name)
        })
        .unwrap_or_else(|| panic!("the window bar's switcher offers {name}"));
    ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action { id: slot.id });
    app.settle();
}

#[test]
fn the_bars_view_switcher_navigates_live_and_preserves_document() {
    use crate::model::View;
    let mut app = TestApp::<CalendarShell>::new().unwrap();
    app.settle();
    let before = app.peer_document().get_heads();
    choose_view_in_bar(&mut app, View::Week);
    assert_eq!(bar_view(), Some(View::Week.display_label()));
    choose_view_in_bar(&mut app, View::Month);
    assert_eq!(bar_view(), Some(View::Month.display_label()));
    assert_eq!(app.peer_document().get_heads(), before);
    assert!(!app.undo(), "view changes create no document undo step");
}

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
        "read-calendar",
        "set-visibility",
    ] {
        assert!(surface.contains(needle), "missing {needle} in {surface}");
    }
    let mut driven = TestApp::<CalendarShell>::drive("month").expect("drive");
    let local = driven
        .effect_on(
            &crate::actions::calendar_resource_id(crate::actions::LOCAL_CALENDAR_ID),
            &CalendarEffect::ReadCalendar {
                id: crate::actions::LOCAL_CALENDAR_ID.into(),
            },
        )
        .expect("read the calendar on this device")
        .value
        .expect("a calendar's read answers it");
    assert_eq!(local["visible"], true, "{local:#}");
    assert!(local["events"].as_u64().is_some(), "{local:#}");
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

#[test]
fn a_new_project_takes_the_next_colour_no_project_has() {
    let mut calendar = Replica::<Calendar>::new();
    let colours = |calendar: &Replica<Calendar>| -> Vec<Palette> {
        calendar
            .state()
            .unwrap()
            .projects
            .iter()
            .map(|project| project.color)
            .collect()
    };
    for name in ["Work", "Personal", "Health"] {
        calendar
            .apply(Calendar::NEW_PROJECT.with(calendar::NewProject {
                name: name.parse().unwrap(),
            }))
            .unwrap();
    }
    assert_eq!(
        colours(&calendar),
        [Palette::Indigo, Palette::Emerald, Palette::Amber],
        "each new project has its own colour"
    );

    let personal = calendar.state().unwrap().projects[1].id;
    calendar
        .apply_confirmed(Calendar::DELETE_PROJECT.with(key(personal)))
        .unwrap();
    calendar
        .apply(Calendar::NEW_PROJECT.with(calendar::NewProject {
            name: "Garden".parse().unwrap(),
        }))
        .unwrap();
    assert!(colours(&calendar).contains(&Palette::Emerald));

    for index in 0..Palette::ALL.len() - 3 {
        calendar
            .apply(Calendar::NEW_PROJECT.with(calendar::NewProject {
                name: format!("Project {index}").parse().unwrap(),
            }))
            .unwrap();
    }
    let mut taken = colours(&calendar);
    taken.sort_by_key(|palette| palette.index());
    assert_eq!(taken, Palette::ALL, "every colour once before any twice");
    calendar
        .apply(Calendar::NEW_PROJECT.with(calendar::NewProject {
            name: "One more".parse().unwrap(),
        }))
        .unwrap();
    assert_eq!(colours(&calendar).last(), Some(&Palette::Indigo));
}

#[test]
fn verbs_take_along_what_hangs_off_an_item() {
    let mut calendar = Replica::<Calendar>::new();
    calendar
        .apply(Calendar::NEW_PROJECT.with(calendar::NewProject {
            name: "Work".parse().unwrap(),
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

    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry
            .invoker
            .as_ref()
            .is_some_and(|invoker| invoker.actor == "agent:claude")
    }));
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
            node.role() == ui::accesskit::Role::TreeItem
                && node.toggled().is_some()
                && node.label() == Some(name.as_str())
        })
        .map(|(id, _)| id)
        .expect("the calendar's checked row in the sidebar");
    app.harness()
        .perform_a11y_action(checkbox, &ui::a11y::Action::Activate)
        .expect("check the row");
    app.settle();
    let display = app.settings::<crate::display::CalendarDisplay>().get();
    assert_eq!(display.visible.get(&calendar), Some(&false));
    assert!(
        !app.state().events.is_empty(),
        "hiding changes no calendar content"
    );
}

#[test]
fn effects_that_cannot_run_are_refused_with_their_reason() {
    let mut app = TestApp::<CalendarShell>::new().expect("test app");
    let sync = format!("{:?}", app.effect(&CalendarEffect::Sync).unwrap_err());
    assert!(sync.contains("account"), "{sync}");
    let export = format!("{:?}", app.effect(&CalendarEffect::ExportIcs).unwrap_err());
    assert!(export.contains("export"), "{export}");
}

#[test]
fn a_host_profile_window_uses_that_profiles_accounts_credentials_and_sync_health() {
    let profiles = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    for (profile, name) in profiles.iter().zip(["first", "second"]) {
        crate::AccountStore {
            accounts: vec![crate::account::CalendarAccount::new(
                crate::account::CalDavConfig {
                    display_name: name.into(),
                    server_url: "http://127.0.0.1:9/".into(),
                    username: format!("{name}-dav"),
                    password: format!("{name}-fixture-password"),
                    ..Default::default()
                },
            )],
        }
        .persist_in(profile.path())
        .unwrap();
    }
    for (profile, name) in profiles.iter().zip(["first", "second"]) {
        let mut app = TestApp::<CalendarShell>::open_data_dir(profile.path()).expect("test app");
        let configured = app
            .with_widget(|surface: &mut CalendarApp| surface.configured_for_test().clone())
            .unwrap();
        let [account] = configured.accounts.as_slice() else {
            panic!("the profile's one account, got {:?}", configured.accounts);
        };
        assert_eq!(account.config.username, format!("{name}-dav"));
        assert_eq!(account.config.password, format!("{name}-fixture-password"));
        let _ = app.effect(&CalendarEffect::Sync);
        let health = accounts::AccountSyncStatusStore::load_in(profile.path());
        assert!(
            health
                .get(&accounts::AccountId::from(account.id.as_str()))
                .is_some(),
            "sync health is kept in the window's profile"
        );
    }
}

mod person_accounts {
    use super::*;
    use accounts::person::{ACCOUNTS_OWNER, PersonAccounts};

    pub(super) struct AccountsOwner;

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

    #[test]
    fn the_persons_caldav_accounts_reach_the_calendar_from_their_record() {
        let mut settings = TestApp::<AccountsOwner>::for_person(&person()).unwrap();
        let mut calendar = TestApp::<CalendarShell>::for_person(&person()).unwrap();
        let kept = settings.shared_person::<PersonAccounts>().unwrap();
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

mod tasks_screen {
    use super::*;
    use tilo_actions::{ActionKind, OperationKind, RetryPolicy};
    use ui::accesskit::{Node, NodeId, Role};

    fn sample_calendar() -> TestApp<CalendarShell> {
        let mut app = TestApp::<CalendarShell>::new().expect("live test app");
        let sample = crate::model::seed_demo(chrono::Local::now().date_naive());
        app.invoke(Invoke::group(sample_invocations(&sample)))
            .expect("seed through the calendar door");
        app.call("show-month", serde_json::json!({}))
            .expect("Month view");
        app
    }

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

    fn named(app: &mut TestApp<CalendarShell>, name: &str) -> Option<(NodeId, Node)> {
        app.harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.label() == Some(name))
    }

    fn choose_place(app: &mut TestApp<CalendarShell>, name: &str) {
        let (id, _) = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.role() == Role::TreeItem && node.label() == Some(name))
            .unwrap_or_else(|| panic!("no place named {name} in the sidebar"));
        app.harness()
            .perform_a11y_action(id, &ui::a11y::Action::Activate)
            .unwrap_or_else(|error| panic!("choose {name}: {error:?}"));
        app.settle();
    }

    fn activate(app: &mut TestApp<CalendarShell>, name: &str) {
        let (id, _) = named(app, name).unwrap_or_else(|| panic!("no control named {name}"));
        app.harness()
            .perform_a11y_action(id, &ui::a11y::Action::Activate)
            .unwrap_or_else(|error| panic!("activate {name}: {error:?}"));
        app.settle();
    }

    #[test]
    fn show_tasks_is_an_idempotent_view_on_the_root() {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        let resources = app.harness().resources();
        let root = resources
            .iter()
            .find(|resource| resource.id.as_str() == "calendar.calendar")
            .expect("the calendar root is published");
        let show = root
            .actions
            .iter()
            .find(|action| action.id == "show-tasks")
            .expect("show-tasks is published on the root");
        assert_eq!(show.label, "Show tasks");
        assert_eq!(show.kind, OperationKind::View);
        assert_eq!(show.contract.kind, ActionKind::Read);
        assert_eq!(show.contract.retry, RetryPolicy::Idempotent);
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
    fn the_actions_the_manifest_offers_while_closed_are_published() {
        use tilo_app_package::manifest::{Manifest, OfferKind, OfferOperation};
        let manifest =
            Manifest::from_toml_str(include_str!("../../tilo-app.toml")).expect("the manifest");
        assert!(!manifest.offers.is_empty(), "Calendar offers actions");
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        let resources = app.harness().resources();
        for offer in &manifest.offers {
            let action = resources
                .iter()
                .find(|resource| resource.id.as_str() == offer.resource)
                .and_then(|resource| resource.actions.iter().find(|a| a.id == offer.action))
                .unwrap_or_else(|| panic!("{}#{} is published", offer.resource, offer.action));
            assert_eq!(action.label, offer.label);
            let kind = match offer.kind {
                OfferKind::Read => ActionKind::Read,
                OfferKind::Undoable => ActionKind::Undoable,
                OfferKind::Outside => ActionKind::Outside,
            };
            assert_eq!(action.contract.kind, kind, "{}", offer.action);
            let operation = match offer.operation {
                OfferOperation::Document => OperationKind::Document,
                OfferOperation::Effect => OperationKind::Effect,
                OfferOperation::View => OperationKind::View,
            };
            assert_eq!(action.kind, operation, "{}", offer.action);
            assert_eq!(
                offer.confirm,
                action.contract.confirmation != tilo_actions::ConfirmationPolicy::None,
                "{}",
                offer.action
            );
            assert!(
                action.params().is_empty(),
                "{} takes no parameters",
                offer.action
            );
        }
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
        let failure = app.call(
            "new-task",
            serde_json::json!({ "task": { "title": "Refused task" } }),
        );
        assert!(
            failure.is_err(),
            "view permission never admits a task mutation"
        );
        assert!(app.state().tasks.is_empty());
    }

    #[test]
    fn show_tasks_lands_on_the_inbox_task_list() {
        let mut app = sample_calendar();
        assert!(task_list(&mut app).is_none(), "Month shows no task list");
        assert!(
            ui::window_toolbar::declared()
                .and_then(|toolbar| toolbar.views)
                .is_some_and(|views| views
                    .choices
                    .iter()
                    .any(|choice| choice.label == crate::model::View::Tasks.display_label())),
            "the bar's Tasks tab is not the task list"
        );
        choose_place(&mut app, "Today");

        app.call("show-tasks", serde_json::json!({}))
            .expect("show-tasks");
        let items = task_list(&mut app).unwrap_or_else(|| {
            panic!(
                "the Tasks screen publishes its list: {:?}",
                app.harness()
                    .a11y_nodes()
                    .iter()
                    .filter_map(|(_, n)| n.label().map(str::to_owned))
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
        assert_eq!(
            bar_view(),
            Some(crate::model::View::Tasks.display_label()),
            "the bar's Tasks tab is chosen"
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

    #[test]
    fn the_tasks_tab_and_show_tasks_reach_the_same_list() {
        let mut by_tab = sample_calendar();
        choose_place(&mut by_tab, "Inbox");
        choose_view_in_bar(&mut by_tab, crate::model::View::Tasks);
        let mut by_action = sample_calendar();
        by_action
            .call("show-tasks", serde_json::json!({}))
            .expect("show-tasks");
        let tab = task_list(&mut by_tab).expect("the tab shows the task list");
        assert_eq!(Some(tab), task_list(&mut by_action));
    }

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

    #[test]
    fn a_task_the_workspace_refuses_is_not_said_to_be_added() {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        app.shared_in_workspace().expect("shared");
        app.call("show-tasks", serde_json::json!({}))
            .expect("show-tasks");
        let heads = app.read_document(|doc| doc.clone().get_heads());
        activate(&mut app, "Add a task to Inbox");
        app.harness().type_text("Buy milk");
        app.harness().key("Enter");
        app.settle();
        assert!(app.workspace_publishes().expect("published"));
        assert!(
            named(&mut app, "Added Buy milk").is_none(),
            "nothing is said done before the workspace answers"
        );
        let changes: Vec<[u8; 32]> = app.read_document(|doc| {
            let mut doc = doc.clone();
            doc.get_changes(&heads)
                .into_iter()
                .map(|change| change.hash().0)
                .collect()
        });
        assert!(
            app.refused_by_workspace(&changes, "This workspace doesn't allow that.")
                .unwrap()
        );
        app.workspace_keeps();
        assert!(app.state().tasks.is_empty(), "the refused task is gone");
        assert!(named(&mut app, "Added Buy milk").is_none());
        assert!(
            named(
                &mut app,
                "Calendar didn't keep that: This workspace doesn't allow that."
            )
            .is_some(),
            "the status says the workspace refused it"
        );
    }

    #[test]
    fn a_task_is_said_added_once_the_workspace_keeps_it() {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        app.shared_in_workspace().expect("shared");
        app.call("show-tasks", serde_json::json!({}))
            .expect("show-tasks");
        activate(&mut app, "Add a task to Inbox");
        app.harness().type_text("Buy milk");
        app.harness().key("Enter");
        app.settle();
        assert!(app.workspace_publishes().expect("published"));
        assert!(named(&mut app, "Added Buy milk").is_none());
        app.workspace_keeps();
        assert!(
            named(&mut app, "Added Buy milk").is_some(),
            "the workspace kept it: it is said done"
        );
    }

    #[test]
    fn a_task_on_this_device_alone_is_said_added_at_once() {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        app.call("show-tasks", serde_json::json!({}))
            .expect("show-tasks");
        activate(&mut app, "Add a task to Inbox");
        app.harness().type_text("Buy milk");
        app.harness().key("Enter");
        app.settle();
        app.settle();
        assert!(named(&mut app, "Added Buy milk").is_some());
    }
}

#[test]
fn the_calendar_reads_its_events_and_one_in_full() {
    let mut app = TestApp::<CalendarShell>::new().expect("test app");
    let id = Uuid::new_v4();
    app.invoke(Calendar::NEW_EVENT.with(calendar::NewEvent {
        event: EventData {
            id: id.to_string(),
            title: "Review".into(),
            start: "2026-09-08T09:00".into(),
            end: "2026-09-08T10:00".into(),
            location: "Room 4".into(),
            ..EventData::default()
        },
    }))
    .expect("create");
    app.invoke(Invoke::from(
        notes_path(id)
            .perform(&RichTextAction::AppendParagraph {
                text: "Bring the mockups".into(),
            })
            .expect("a notes edit"),
    ))
    .expect("append");
    let listed = app.call("read", serde_json::json!({})).unwrap().unwrap();
    let event = &listed["events"][0];
    assert_eq!(event["title"], "Review");
    assert_eq!(event["location"], "Room 4");
    let item = event["item"].as_str().unwrap().to_owned();
    let full = app
        .call("read", serde_json::json!({ "item": item }))
        .unwrap()
        .unwrap();
    assert_eq!(full["notes"], "Bring the mockups");
    assert_eq!(full["start"], "2026-09-08 09:00:00");
    assert!(
        app.call("read", serde_json::json!({ "item": "nothing" }))
            .is_err()
    );
}

#[test]
fn set_start_moves_an_event_keeping_its_length() {
    let mut app = TestApp::<CalendarShell>::new().expect("test app");
    app.invoke(new_event(
        "Planning",
        "2026-09-08T09:00",
        "2026-09-08T10:30",
    ))
    .expect("create");
    let event = state_of(&app).events[0].clone();
    app.call_on(
        &crate::actions::event_resource_id(event.id),
        "set-start",
        serde_json::json!({ "start": "2026-09-10T14:00:00" }),
    )
    .expect("set-start");
    let moved = state_of(&app).event(event.id).expect("event").clone();
    let day = chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
    assert_eq!(moved.start, at(day, 14, 0));
    assert_eq!(moved.end, at(day, 15, 30));
}

#[test]
fn export_ics_content_keeps_the_calendar_or_one_event_as_content() {
    let mut app = TestApp::<CalendarShell>::new().expect("test app");
    for (title, start, end) in [
        ("Standup", "2026-09-08T09:00", "2026-09-08T09:15"),
        ("Review", "2026-09-09T13:00", "2026-09-09T14:00"),
    ] {
        app.invoke(new_event(title, start, end)).expect("create");
    }
    let text = |app: &TestApp<CalendarShell>, answer: &serde_json::Value| {
        let hash = ui::tilo_actions::content_hash(answer["content"].as_str().unwrap()).unwrap();
        String::from_utf8(app.content().read(hash).unwrap()).unwrap()
    };
    let all = app
        .call("export-ics-content", serde_json::json!({}))
        .expect("export")
        .expect("an answer");
    assert_eq!(all["events"], 2);
    let ics = text(&app, &all);
    assert!(ics.starts_with("BEGIN:VCALENDAR"), "{ics}");
    assert!(ics.contains("SUMMARY:Standup") && ics.contains("SUMMARY:Review"));
    let review = state_of(&app)
        .events
        .iter()
        .find(|event| event.title == "Review")
        .expect("review")
        .id;
    let one = app
        .call(
            "export-ics-content",
            serde_json::json!({ "event": review.to_string() }),
        )
        .expect("export one")
        .expect("an answer");
    assert_eq!(one["events"], 1);
    let ics = text(&app, &one);
    assert!(ics.contains("SUMMARY:Review") && !ics.contains("SUMMARY:Standup"));
}

#[test]
fn the_window_bars_sidebar_button_flips_the_sidebar() {
    ui::framework::testing::check_window_sidebar_button(
        ui::framework::testing::TestApp::<CalendarShell>::new,
    )
    .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn a_past_version_reads_and_shows_its_event() {
    let mut app = TestApp::<CalendarShell>::new().expect("test app");
    let day = ui::clock::today().format("%Y-%m-%d");
    app.call(
        "new-event",
        serde_json::json!({ "event": {
            "title": "Kumquat parade",
            "start": format!("{day}T09:00"),
            "end": format!("{day}T10:00"),
        } }),
    )
    .expect("create");
    let past = app.versions().pop().unwrap().hash;
    let event = state_of(&app).events[0].id;
    app.call_on(
        &crate::actions::event_resource_id(event),
        "delete-event",
        serde_json::json!({}),
    )
    .expect("delete");
    assert!(state_of(&app).events.is_empty());

    let read = app
        .call("read", serde_json::json!({ "version": past.to_string() }))
        .unwrap()
        .unwrap();
    assert!(
        read["content"].to_string().contains("Kumquat parade"),
        "{read}"
    );

    let texts = app.past_version(&[past]).unwrap().texts();
    assert!(
        texts.iter().any(|text| text.contains("Kumquat parade")),
        "{texts:?}"
    );
}

#[test]
fn an_agent_edits_the_open_events_notes_through_their_own_resource() {
    let mut app = TestApp::<CalendarShell>::new().expect("test app");
    app.harness().key("n");
    let id = state_of(&app).events.first().expect("n makes an event").id;
    app.invoke_child::<RichText>(
        &crate::resource::notes_resource_id(&id),
        Invoke::apply(&RichTextAction::ReplaceBody {
            markdown: "Sponsors at four".into(),
        })
        .expect("a notes edit"),
    )
    .expect("the notes are published at their own address");
    assert!(state_of(&app).notes_text(id).contains("Sponsors at four"));
}
