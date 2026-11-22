use super::*;
use crate::actions::EventData;
use crate::model::key;
use ui::tilo_resource::{Invoke, Replica};

fn apply(calendar: &mut Replica<Calendar>, invoke: Invoke<Calendar>) {
    calendar.apply(invoke).expect("applies");
}

tilo_test_env::scenario! {
    /// The outbox: what is owed to which calendar, and what no longer is.
    fn the_outbox_drains_moves_and_drops_what_is_done;

    phase the_differences_drain_to_the_calendars_this_device_holds;
    {
        let accounts = CalendarAccounts::sample();
        let (_, calendar) = accounts.all_calendars().next().expect("a sample calendar");
        let mut replica = Replica::<Calendar>::new();
        let mut held = crate::resource::event_at(
            "Standup",
            crate::resource::at(jiff::civil::Date::new(2026, 9, 8).unwrap(), 9, 0),
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

    phase a_move_to_another_calendar_deletes_there_then_uploads_here;
    {
        let accounts = CalendarAccounts::sample();
        let mut calendars = accounts
            .all_calendars()
            .map(|(_, calendar)| calendar.id.clone());
        let first = calendars.next().expect("a sample calendar");
        let second = calendars.next().expect("a second sample calendar");
        let mut replica = Replica::<Calendar>::new();
        let mut event = crate::resource::event_at(
            "Review",
            crate::resource::at(jiff::civil::Date::new(2026, 9, 8).unwrap(), 9, 0),
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

    phase what_no_longer_needs_doing_leaves_the_queue;
    {
        let accounts = CalendarAccounts::sample();
        let (_, calendar) = accounts.all_calendars().next().expect("a sample calendar");
        let mut replica = Replica::<Calendar>::new();
        let mut event = crate::resource::event_at(
            "Standup",
            crate::resource::at(jiff::civil::Date::new(2026, 9, 8).unwrap(), 9, 0),
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
}

/// An event's guests are kept under their ids, in no order of their own: read back from the
/// document they stand in the order of their ids, whatever order the entry named them in.
/// What a server holds of an event is told by a fingerprint, which must not change with that.
#[test]
fn an_adopted_event_is_owed_no_upload_whatever_order_its_guests_stand_in() {
    let accounts = CalendarAccounts::sample();
    let (_, held) = accounts.all_calendars().next().expect("a sample calendar");
    let guest = |id: u128, name: &str, email: &str| crate::model::Attendee {
        id: uuid::Uuid::from_u128(id),
        name: name.into(),
        email: email.into(),
        status: crate::model::InviteStatus::NeedsAction,
    };
    for against_their_ids in [false, true] {
        let mut event = crate::resource::event_at(
            "Dinner",
            crate::resource::at(jiff::civil::Date::new(2026, 9, 8).unwrap(), 19, 0),
        );
        event.calendar_id = Some(held.id.clone());
        let mut guests = vec![
            guest(1, "Ada", "ada@example.test"),
            guest(2, "Bo", "bo@example.test"),
        ];
        if against_their_ids {
            guests.reverse();
        }
        event.organizer = Some(uuid::Uuid::from_u128(1));
        event.attendees = guests.into();
        let mut replica = Replica::<Calendar>::new();
        apply(
            &mut replica,
            Calendar::ADOPT_REMOTE_EVENT.with(calendar::AdoptRemoteEvent {
                event: EventData::from_event(&event),
                at: RemoteAt {
                    calendar: held.id.clone(),
                    href: "/dav/dinner.ics".into(),
                    etag: Some("\"1\"".into()),
                },
            }),
        );
        assert_eq!(replica.state().unwrap().uploads.pending().count(), 0);
        // Any other change of the calendar reads the event back from the document.
        apply(
            &mut replica,
            Calendar::PULLED.with(calendar::Pulled {
                calendar: held.id.clone(),
                token: "token-1".into(),
            }),
        );
        let state = replica.state().unwrap();
        assert_eq!(
            state.uploads.pending().count(),
            0,
            "nobody changed the event, so nothing is owed its server (guests named against \
             the order of their ids: {against_their_ids})"
        );
        assert!(uploads(&state, &accounts).is_empty());
    }
}
