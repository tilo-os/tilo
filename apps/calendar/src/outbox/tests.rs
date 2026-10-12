use super::*;
use crate::actions::EventData;
use crate::model::key;
use ui::tilo_resource::{Invoke, Replica};

fn apply(calendar: &mut Replica<Calendar>, invoke: Invoke<Calendar>) {
    calendar.apply(invoke).expect("applies");
}

/// Only what an account on this device can reach is drained here; the
/// rest waits for a device that holds its account. What a server
/// confirmed is not sent again, and a deleted event is deleted there.
#[test]
fn the_differences_drain_to_the_calendars_this_device_holds() {
    let accounts = AccountStore::sample();
    let (_, calendar) = accounts.all_calendars().next().expect("a sample calendar");
    let mut replica = Replica::<Calendar>::new();
    let mut held = crate::resource::event_at(
        "Standup",
        crate::resource::at(chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(), 9, 0),
    );
    held.calendar_id = Some(calendar.id.clone());
    let mut elsewhere = held.clone();
    elsewhere.id = uuid::Uuid::new_v4();
    elsewhere.calendar_id = Some("another-device/calendar".into());
    for event in [&held, &elsewhere] {
        apply(
            &mut replica,
            Calendar::NEW_EVENT.with(calendar::NewEvent {
                event: EventData::from_event(event),
            }),
        );
    }
    let state = replica.state().unwrap();
    assert_eq!(
        state.uploads.pending().count(),
        2,
        "both new remote events are queued, whoever holds their account"
    );
    assert_eq!(pending_copies(&state).len(), 0, "nothing was uploaded yet");
    let jobs = uploads(&state, &accounts);
    assert_eq!(jobs.len(), 1, "only the calendar this device holds");
    assert_eq!(jobs[0].0, held.id);
    assert_eq!(jobs[0].1.calendar.id, calendar.id);
    assert_eq!(jobs[0].1.href, None, "a new event is created there");

    let at = RemoteAt {
        calendar: calendar.id.clone(),
        href: format!("/dav/{}.ics", held.id),
        etag: Some("\"1\"".into()),
    };
    apply(
        &mut replica,
        Calendar::SERVER_APPLIED.with(calendar::ServerApplied {
            event: held.id,
            at: at.clone(),
            holds: fingerprint(&held),
        }),
    );
    let state = replica.state().unwrap();
    assert!(uploads(&state, &accounts).is_empty(), "the server holds it");

    apply(
        &mut replica,
        Invoke::from(
            Calendar::paths()
                .events
                .item(&key(held.id))
                .title
                .set("Standup (moved)".into()),
        ),
    );
    let state = replica.state().unwrap();
    let jobs = uploads(&state, &accounts);
    assert_eq!(jobs.len(), 1, "an edit is uploaded");
    assert_eq!(jobs[0].1.href.as_deref(), Some(at.href.as_str()));
    assert_eq!(
        jobs[0].1.etag, at.etag,
        "guarded by the version it replaces"
    );
    assert_eq!(pending_copies(&state).len(), 1);

    apply(&mut replica, Calendar::DELETE_EVENT.with(key(held.id)));
    let state = replica.state().unwrap();
    assert!(
        uploads(&state, &accounts).is_empty(),
        "a deleted event is not uploaded"
    );
    let jobs = deletes(&state, &accounts);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].0, held.id);
    assert_eq!(jobs[0].1.href.as_deref(), Some(at.href.as_str()));

    apply(
        &mut replica,
        Calendar::SERVER_REMOVED.with(calendar::ServerRemoved { item: held.id }),
    );
    let state = replica.state().unwrap();
    assert!(
        deletes(&state, &accounts).is_empty(),
        "a confirmed delete is not sent again"
    );
}

/// An event moved to another calendar is deleted from the old one's
/// server first, then created on the new one's.
#[test]
fn a_move_to_another_calendar_deletes_there_then_uploads_here() {
    let accounts = AccountStore::sample();
    let mut calendars = accounts
        .all_calendars()
        .map(|(_, calendar)| calendar.id.clone());
    let first = calendars.next().expect("a sample calendar");
    let second = calendars.next().expect("a second sample calendar");
    let mut replica = Replica::<Calendar>::new();
    let mut event = crate::resource::event_at(
        "Review",
        crate::resource::at(chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(), 9, 0),
    );
    event.calendar_id = Some(first.clone());
    apply(
        &mut replica,
        Calendar::ADOPT_REMOTE_EVENT.with(calendar::AdoptRemoteEvent {
            event: EventData::from_event(&event),
            at: RemoteAt {
                calendar: first,
                href: "/dav/review.ics".into(),
                etag: None,
            },
        }),
    );
    let state = replica.state().unwrap();
    assert!(
        uploads(&state, &accounts).is_empty(),
        "adopted as the server holds it"
    );
    assert!(deletes(&state, &accounts).is_empty());

    apply(
        &mut replica,
        Calendar::MOVE_TO_CALENDAR.with((
            key(event.id),
            Some(second.clone()),
            crate::model::Palette::Amber,
        )),
    );
    let state = replica.state().unwrap();
    assert!(
        uploads(&state, &accounts).is_empty(),
        "the old copy goes first"
    );
    assert_eq!(deletes(&state, &accounts).len(), 1);
    apply(
        &mut replica,
        Calendar::SERVER_REMOVED.with(calendar::ServerRemoved { item: event.id }),
    );
    let state = replica.state().unwrap();
    let jobs = uploads(&state, &accounts);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].1.calendar.id, second);
    assert_eq!(jobs[0].1.href, None);
}

/// An event deleted before its upload ran is not uploaded, and nothing is
/// deleted on a server that never held it; settled entries are kept only
/// as many as the calendar keeps.
#[test]
fn what_no_longer_needs_doing_leaves_the_queue() {
    let accounts = AccountStore::sample();
    let (_, calendar) = accounts.all_calendars().next().expect("a sample calendar");
    let mut replica = Replica::<Calendar>::new();
    let mut event = crate::resource::event_at(
        "Standup",
        crate::resource::at(chrono::NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(), 9, 0),
    );
    event.calendar_id = Some(calendar.id.clone());
    apply(
        &mut replica,
        Calendar::NEW_EVENT.with(calendar::NewEvent {
            event: EventData::from_event(&event),
        }),
    );
    assert_eq!(replica.state().unwrap().uploads.pending().count(), 1);
    apply(&mut replica, Calendar::DELETE_EVENT.with(key(event.id)));
    let state = replica.state().unwrap();
    assert_eq!(
        state.uploads.pending().count(),
        0,
        "the upload is forgotten"
    );
    assert_eq!(state.deletes.pending().count(), 0, "no server holds it");
}
