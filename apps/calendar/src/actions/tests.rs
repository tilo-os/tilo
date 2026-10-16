use super::*;
use crate::outbox::{deletes, uploads};
use crate::resource::{CalendarEffect, RemoteAt, calendar, fingerprint, notes_path};
use tilo_actions::{ActionRegistry, EncodedAction, Resource};
use tilo_document::RichTextAction;
use ui::tilo_resource::{Invoke, Replica, Resource as _};

/// The sample calendar, written through its own verbs.
fn seeded() -> Replica<Calendar> {
    let mut calendar = Replica::<Calendar>::new();
    let sample =
        crate::model::seed_demo(chrono::NaiveDate::from_ymd_opt(2026, 6, 30).expect("a date"));
    for invoke in crate::resource::sample_invocations(&sample) {
        calendar.apply(invoke).expect("the sample applies");
    }
    calendar
}

fn seeded_doc() -> ui::automerge::AutoCommit {
    seeded().doc().clone()
}

fn read(doc: &ui::automerge::AutoCommit) -> Calendar {
    crate::resource::read_doc(doc)
}

fn registry() -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    Calendar::register(&mut registry);
    registry
}

fn encoded(invoke: &Invoke<Calendar>) -> EncodedAction {
    let action = invoke.build(1_700_000_000_000).unwrap();
    ui::tilo_resource::encode::<Calendar>(&action).unwrap()
}

/// The calendar's published tree for `doc`, with its effects.
fn resources_for(doc: &ui::automerge::AutoCommit) -> Vec<Resource> {
    let mut root =
        ui::tilo_resource::publish::<Calendar>("calendar.calendar", "Calendar", &read(doc));
    root.actions
        .extend(<CalendarEffect as ui::tilo_resource::Effect>::specs());
    vec![root]
}

fn first_event(doc: &ui::automerge::AutoCommit) -> Event {
    read(doc).events[0].clone()
}

/// An event as its JSON form, starting at `start` for an hour.
fn event_data(title: &str, start: &str, end: &str) -> EventData {
    EventData {
        id: Uuid::new_v4().to_string(),
        title: title.into(),
        start: start.into(),
        end: end.into(),
        ..EventData::default()
    }
}

ui::action_contract_tests! {
    seed: seeded_doc(),
    registry: registry(),
    resources: |doc| resources_for(doc),
    palette: ["New event", "New task", "Delete event", "Reschedule"],
    replay: [
        // Completion is an absolute value, so replay does not depend on the
        // receiving replica's current task state.
        finish_task => |doc| {
            let task = read(doc).tasks.iter().find(|t| !t.done).unwrap().id;
            encoded(&Invoke::from(
                Calendar::paths().tasks.item(&key(task)).done.set(true),
            ))
        },
        reschedule => |doc| {
            let event = first_event(doc);
            encoded(&Calendar::RESCHEDULE.with((
                key(event.id),
                event.start + chrono::Duration::hours(1),
                event.end + chrono::Duration::hours(1),
            )))
        },
        edit_notes => |doc| encoded(&Invoke::from(
            notes_path(first_event(doc).id)
                .perform(&RichTextAction::AppendParagraph {
                    text: "Résumé ✓".to_string(),
                })
                .unwrap(),
        )),
        invite => |doc| encoded(&Calendar::INVITE.with((
            key(first_event(doc).id),
            "sam@example.com".to_owned(),
            "Sam".to_owned(),
        ))),
    ],
    undo: [
        delete_event => |doc| encoded(&Calendar::DELETE_EVENT.with(key(first_event(doc).id))),
        new_event => |_| encoded(&Calendar::NEW_EVENT.with(calendar::NewEvent {
            event: event_data("Undoable", "2026-09-06T09:00", "2026-09-06T10:00"),
        })),
    ],
}

/// The calendar publishes its events, tasks and projects as its items, each
/// with its verbs; an item past the published page still resolves by id,
/// and a deleted one is refused rather than redirected.
#[test]
fn the_calendar_publishes_its_items_and_resolves_each_by_id() {
    let doc = seeded_doc();
    let resources = resources_for(&doc);
    let root = &resources[0];
    assert_eq!(root.kind, "calendar.calendar");
    let ids: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
    for verb in [
        "new-event",
        "new-task",
        "new-project",
        "import-items",
        "field-edit",
    ] {
        assert!(ids.contains(&verb), "{verb} in {ids:?}");
    }
    let state = read(&doc);
    for (item, verbs) in [
        (
            event_resource_id(state.events[0].id),
            &["delete-event", "reschedule", "move-to-calendar", "invite"][..],
        ),
        (task_resource_id(state.tasks[0].id), &["delete-task"][..]),
        (
            project_resource_id(state.projects[0].id),
            &["delete-project"][..],
        ),
    ] {
        let resource = Calendar::child(&state, &item).unwrap_or_else(|| panic!("{item} resolves"));
        for verb in verbs {
            assert!(
                resource.actions.iter().any(|action| action.id == *verb),
                "{item} offers {verb}"
            );
        }
    }

    let mut calendar = Replica::<Calendar>::from_doc(doc);
    let late = Uuid::new_v4();
    calendar
        .apply(Calendar::NEW_EVENT.with(calendar::NewEvent {
            event: EventData {
                id: late.to_string(),
                ..event_data("Late", "2026-12-24T18:00", "2026-12-24T22:00")
            },
        }))
        .unwrap();
    let state = calendar.state().unwrap();
    assert!(Calendar::child(&state, &event_resource_id(late)).is_some());
    calendar
        .apply(Calendar::DELETE_EVENT.with(key(late)))
        .unwrap();
    assert!(
        Calendar::child(&calendar.state().unwrap(), &event_resource_id(late)).is_none(),
        "a deleted id is refused rather than redirected"
    );
}

/// What a verb or a peer's edit may not do is refused whole, before
/// anything is written.
#[test]
fn invalid_and_hostile_changes_are_refused_atomically() {
    let mut calendar = seeded();
    let before = calendar.heads();
    let event = calendar.state().unwrap().events[0].clone();

    // An event filed under a project the calendar does not hold.
    let mut stray = event_data("Stray", "2026-09-06T09:00", "2026-09-06T10:00");
    stray.project = Uuid::new_v4().to_string();
    assert!(
        calendar
            .apply(Calendar::NEW_EVENT.with(calendar::NewEvent { event: stray }))
            .is_err()
    );
    // An end before the start, whether moved or written.
    assert!(
        calendar
            .apply(Calendar::RESCHEDULE.with((
                key(event.id),
                event.start,
                event.start - chrono::Duration::minutes(1),
            )))
            .is_err()
    );
    assert!(
        calendar
            .apply(Invoke::from(
                Calendar::paths()
                    .events
                    .item(&key(event.id))
                    .end
                    .set(event.start - chrono::Duration::minutes(1)),
            ))
            .is_err()
    );
    // A blank project name; an email that is no address.
    let project = calendar.state().unwrap().projects[0].id;
    assert!(
        calendar
            .apply(Invoke::from(
                Calendar::paths()
                    .projects
                    .item(&key(project))
                    .name
                    .set("   ".into()),
            ))
            .is_err()
    );
    assert!(
        calendar
            .apply(Calendar::INVITE.with((
                key(event.id),
                "not an address".to_owned(),
                String::new()
            )))
            .is_err()
    );
    // A server's own records cannot be written as field edits.
    assert!(
        calendar
            .apply(Invoke::from(
                Calendar::paths()
                    .remote
                    .key(event.id.to_string())
                    .holds
                    .set("forged".into()),
            ))
            .is_err(),
        "what a server confirmed is written only by the server's own records"
    );
    // An import with one bad item writes none of them.
    let (mut events, tasks) = import_payload(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:batch-event\r\n\
         SUMMARY:Valid batch event\r\nDTSTART:20260919T090000\r\nDTEND:20260919T100000\r\n\
         END:VEVENT\r\nBEGIN:VTODO\r\nUID:batch-task\r\nSUMMARY:Valid batch task\r\n\
         END:VTODO\r\nEND:VCALENDAR\r\n",
    )
    .expect("a mixed import");
    events.push(EventData {
        end: "not-a-date".into(),
        ..event_data("Broken", "2026-09-19T09:00", "2026-09-19T10:00")
    });
    assert!(
        calendar
            .apply(Calendar::IMPORT_ITEMS.with(calendar::ImportItems { events, tasks }))
            .is_err()
    );
    assert_eq!(calendar.heads(), before, "nothing was written");
}

/// Edits on two replicas of one event — an attendee's answer, the series'
/// time zone and repeat, a skipped date — all land on both.
#[test]
fn replicated_edits_of_one_event_converge() {
    let mut first = seeded();
    let event = first.state().unwrap().events[0].id;
    first
        .apply(Calendar::INVITE.with((key(event), "sam@example.com".to_owned(), "Sam".to_owned())))
        .unwrap();
    let attendee = first.state().unwrap().event(event).unwrap().attendees[0].id;
    let mut second = first.fork();
    let fields = Calendar::paths().events.item(&key(event));
    first
        .apply(Invoke::from(
            fields
                .attendees
                .item(&key(attendee))
                .status
                .set(InviteStatus::Accepted),
        ))
        .unwrap();
    for edit in [
        fields.timezone.set("Europe/Berlin".into()),
        fields.recurrence_interval.set(2),
        fields.recurrence_count.set(Some(7)),
        fields
            .recurrence_exdates
            .insert(&NaiveDate::from_ymd_opt(2026, 6, 17).unwrap()),
    ] {
        second.apply(Invoke::from(edit)).unwrap();
    }
    first.sync_with(&mut second).unwrap();
    let left = first.state().unwrap().event(event).cloned().unwrap();
    let right = second.state().unwrap().event(event).cloned().unwrap();
    assert_eq!(left, right);
    assert_eq!(left.timezone, "Europe/Berlin");
    assert_eq!(left.recurrence_interval, 2);
    assert_eq!(left.recurrence_count, Some(7));
    assert_eq!(left.recurrence_exdates.len(), 1);
    assert_eq!(left.attendees[0].status, InviteStatus::Accepted);

    // A removed attendee stays removed whatever a stale replica answered.
    let mut stale = first.fork();
    first
        .apply_confirmed(Invoke::from(fields.attendees.remove(&key(attendee))))
        .unwrap();
    stale
        .apply(Invoke::from(
            fields
                .attendees
                .item(&key(attendee))
                .status
                .set(InviteStatus::Declined),
        ))
        .unwrap();
    first.sync_with(&mut stale).unwrap();
    assert!(
        first
            .state()
            .unwrap()
            .event(event)
            .unwrap()
            .attendees
            .is_empty()
    );
}

#[test]
fn importing_a_duration_through_the_autumn_fold_keeps_actual_instants() {
    // Both a reversed wall interval and a later-fold endpoint after the
    // start need an unambiguous representation, not just a local-order fix.
    for (duration, expected_end) in [("PT30M", "01:15"), ("PT65M", "01:50")] {
        let source = format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:fold-visit@example.com\r\nSUMMARY:Visit\r\nDTSTART;TZID=Europe/Berlin:20261025T024500\r\nDURATION:{duration}\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:other-visit@example.com\r\nSUMMARY:Other\r\nDTSTART:20261026T090000\r\nDTEND:20261026T100000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let mut calendar = Replica::<Calendar>::new();
        calendar
            .apply(crate::resource::import(&source).unwrap())
            .unwrap();
        let state = calendar.state().unwrap();
        assert_eq!(state.events.len(), 2);
        let event = state
            .events
            .iter()
            .find(|event| event.title == "Visit")
            .unwrap();
        let start = crate::timezone::wall_to_utc(&event.timezone, event.start)
            .unwrap()
            .0;
        let end = crate::timezone::wall_to_utc(&event.timezone, event.end)
            .unwrap()
            .0;
        assert_eq!(start.to_rfc3339(), "2026-10-25T00:45:00+00:00");
        assert_eq!(
            end.to_rfc3339(),
            format!("2026-10-25T{expected_end}:00+00:00")
        );
        let exported = crate::ics::export(&state);
        let mut copy = Replica::<Calendar>::new();
        copy.apply(crate::resource::import(&exported).unwrap())
            .unwrap();
        let copy_state = copy.state().unwrap();
        let copied = copy_state
            .events
            .iter()
            .find(|event| event.title == "Visit")
            .unwrap();
        assert_eq!(
            (copied.start, copied.end, &copied.timezone),
            (event.start, event.end, &event.timezone)
        );
    }
}

/// One import is one action, and its payload replays to the same calendar
/// on every replica.
#[test]
fn an_import_replays_as_one_deterministic_action() {
    let mut a = seeded();
    let mut b = a.fork();
    let event = event_data("Imported planning", "2026-09-06T09:00", "2026-09-06T10:00");
    a.apply(Calendar::IMPORT_ITEMS.with(calendar::ImportItems {
        events: vec![event.clone()],
        tasks: Vec::new(),
    }))
    .unwrap();
    let envelope = a.log().last().expect("the import's envelope").clone();
    b.replay(&envelope).unwrap();
    assert_eq!(a.state().unwrap(), b.state().unwrap());
    let landed = a
        .state()
        .unwrap()
        .events
        .iter()
        .find(|candidate| candidate.id.to_string() == event.id)
        .cloned()
        .expect("the imported event landed");
    assert_eq!(read(a.doc()).notes_text(landed.id), "Imported from .ics");
}

/// A pulled event is adopted as its server holds it, so it needs no
/// upload; dropping it removes it with what the server held; a confirmed
/// delete and a sync token are recorded.
#[test]
fn a_pulled_event_is_adopted_without_an_upload() {
    let accounts = crate::AccountStore::sample();
    let remote = accounts
        .all_calendars()
        .next()
        .map(|(_, calendar)| calendar.id.clone())
        .unwrap();
    let mut calendar = Replica::<Calendar>::new();
    let event = EventData {
        calendar_id: remote.clone(),
        ..event_data("Standup", "2026-09-19T09:00", "2026-09-19T10:00")
    };
    let id: Id = event.id.parse().unwrap();
    let at = RemoteAt {
        calendar: remote.clone(),
        href: "/calendars/work/standup.ics".into(),
        etag: Some("\"v1\"".into()),
    };
    calendar
        .apply(
            Calendar::ADOPT_REMOTE_EVENT.with(calendar::AdoptRemoteEvent {
                event: event.clone(),
                at: at.clone(),
            }),
        )
        .unwrap();
    let state = calendar.state().unwrap();
    assert_eq!(
        state.event(id).map(|e| e.title.clone()),
        Some("Standup".into())
    );
    assert!(
        uploads(&state, &accounts).is_empty(),
        "no upload of a pulled event"
    );
    let copy = &state.remote[&id.to_string()];
    assert_eq!(copy.href, at.href);
    assert_eq!(copy.holds, fingerprint(&event.to_event().unwrap()));
    assert!(
        calendar
            .apply(
                Calendar::ADOPT_REMOTE_EVENT.with(calendar::AdoptRemoteEvent {
                    event,
                    at: RemoteAt {
                        calendar: String::new(),
                        ..at
                    },
                })
            )
            .is_err(),
        "a server address names its calendar"
    );

    calendar
        .apply(Calendar::DROP_REMOTE_ITEM.with(calendar::DropRemoteItem { item: id }))
        .unwrap();
    let state = calendar.state().unwrap();
    assert!(state.event(id).is_none());
    assert!(state.remote.is_empty());
    assert!(
        deletes(&state, &accounts).is_empty(),
        "the server already removed it"
    );

    calendar
        .apply(Calendar::PULLED.with(calendar::Pulled {
            calendar: remote.clone(),
            token: "sync-1".into(),
        }))
        .unwrap();
    assert_eq!(calendar.state().unwrap().sync_tokens[&remote], "sync-1");
}

/// Undoing the delete of an event its server already deleted restores it,
/// and its server takes it again.
#[test]
fn undoing_a_delete_the_server_took_uploads_the_event_again() {
    let mut app = ui::framework::testing::TestApp::<crate::resource::CalendarShell>::new().unwrap();
    let accounts = crate::AccountStore::sample();
    let remote = accounts
        .all_calendars()
        .next()
        .map(|(_, calendar)| calendar.id.clone())
        .unwrap();
    let event = EventData {
        calendar_id: remote.clone(),
        ..event_data("Standup", "2026-09-19T09:00", "2026-09-19T10:00")
    };
    let id: Id = event.id.parse().unwrap();
    app.invoke(
        Calendar::ADOPT_REMOTE_EVENT.with(calendar::AdoptRemoteEvent {
            event,
            at: RemoteAt {
                calendar: remote,
                href: "/calendars/work/standup.ics".into(),
                etag: None,
            },
        }),
    )
    .unwrap();
    app.invoke(Calendar::DELETE_EVENT.with(key(id))).unwrap();
    let state = app.read_document(read);
    assert_eq!(deletes(&state, &accounts).len(), 1);
    app.invoke(Calendar::SERVER_REMOVED.with(calendar::ServerRemoved { item: id }))
        .unwrap();
    assert!(app.undo(), "the delete is undone");
    let state = app.read_document(read);
    assert!(state.event(id).is_some());
    let jobs = uploads(&state, &accounts);
    assert_eq!(jobs.len(), 1, "the restored event is uploaded again");
    assert_eq!(jobs[0].1.href, None, "as a new item there");
}

/// An event's and a task's JSON forms round-trip what the calendar holds.
#[test]
fn json_forms_round_trip_events_and_tasks() {
    let state = read(&seeded_doc());
    for event in &state.events {
        let data = EventData::from_event(event);
        let mut back = data.to_event().expect("the form reads back");
        // A reminder the editor does not offer is dropped on the way.
        back.reminder_min = event
            .reminder_min
            .filter(|m| crate::model::REMINDERS.contains(m));
        assert_eq!(&back, event);
    }
    for task in &state.tasks {
        assert_eq!(&TaskData::from_task(task).to_task().unwrap(), task);
    }
    let blank: EventData = serde_json::from_value(serde_json::json!({
        "title": "Coffee", "start": "2026-09-06T09:00", "end": "2026-09-06T09:30"
    }))
    .expect("an event names what it needs, the rest defaults");
    let coffee = EventData {
        id: Uuid::new_v4().to_string(),
        ..blank
    }
    .to_event()
    .expect("a whole event");
    assert_eq!(coffee.timezone, "floating");
    assert_eq!(coffee.recurrence, Recurrence::None);
    assert_eq!(coffee.reminder_min, None);
}
