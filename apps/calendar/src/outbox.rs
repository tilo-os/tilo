//! Telling the servers of this device's accounts what changed.
//!
//! Each remote item carries what its server last confirmed holding
//! ([`Calendar::remote`]). An event on a remote calendar whose content
//! differs from that — new, edited, moved back by an undo — is uploaded; a
//! confirmed copy whose event is gone, or that now lives on another
//! calendar, is deleted from its server. Whoever made the change, every
//! participant's live window holding the account does the upload or the
//! delete and records what the server confirmed (`server-applied`,
//! `server-removed`), so the difference closes and nothing is sent twice;
//! one that fails stays different and is tried again when the calendar
//! next changes.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, Effected, SurfaceCx};

use crate::account::AccountStore;
use crate::caldav::{self, MutationResult, RemoveJob, UploadJob};
use crate::model::{Calendar, Id, RemoteCopy};
use crate::resource::{RemoteAt, calendar, fingerprint};

/// The accounts configured on this device, as the drains read them.
pub(crate) type DeviceAccounts = Rc<RefCell<AccountStore>>;

/// Each event on a remote calendar its server does not hold as it is, whose
/// calendar an account on this device holds.
pub(crate) fn uploads(state: &Calendar, accounts: &AccountStore) -> Vec<(Id, UploadJob)> {
    state
        .events
        .iter()
        .filter_map(|event| {
            let calendar_id = event.calendar_id.as_ref()?;
            let copy = state.remote.get(&event.id.to_string());
            let held = copy.filter(|copy| &copy.calendar == calendar_id);
            if copy.is_some() && held.is_none() {
                // Still on the calendar it left: deleted there first.
                return None;
            }
            if held.is_some_and(|copy| copy.holds == fingerprint(event)) {
                return None;
            }
            let (account, calendar) = accounts
                .all_calendars()
                .find(|(_, calendar)| &calendar.id == calendar_id)?;
            Some((
                event.id,
                UploadJob {
                    config: account.config.clone(),
                    calendar: calendar.clone(),
                    event: event.clone(),
                    href: held
                        .map(|copy| copy.href.clone())
                        .filter(|href| !href.is_empty()),
                    etag: held.and_then(|copy| copy.etag.clone()),
                },
            ))
        })
        .collect()
}

/// Each event a server still holds that the calendar no longer has there,
/// whose calendar an account on this device holds.
pub(crate) fn deletes(state: &Calendar, accounts: &AccountStore) -> Vec<(Id, RemoveJob)> {
    state
        .remote
        .iter()
        .filter(|(_, copy)| !copy.task)
        .filter_map(|(id, copy)| {
            let id = id.parse::<Id>().ok()?;
            let kept = state
                .event(id)
                .is_some_and(|event| event.calendar_id.as_deref() == Some(copy.calendar.as_str()));
            if kept {
                return None;
            }
            let (account, calendar) = accounts
                .all_calendars()
                .find(|(_, calendar)| calendar.id == copy.calendar)?;
            Some((
                id,
                RemoveJob {
                    config: account.config.clone(),
                    calendar: calendar.clone(),
                    id,
                    href: Some(copy.href.clone()).filter(|href| !href.is_empty()),
                    etag: copy.etag.clone(),
                },
            ))
        })
        .collect()
}

/// The remote copy an upload of `event` waits on, if any: what protects the
/// local version from what the server sends meanwhile.
pub(crate) fn pending_copies(state: &Calendar) -> Vec<&RemoteCopy> {
    state
        .events
        .iter()
        .filter_map(|event| {
            let copy = state.remote.get(&event.id.to_string())?;
            (event.calendar_id.as_deref() == Some(copy.calendar.as_str())
                && copy.holds != fingerprint(event))
            .then_some(copy)
        })
        .collect()
}

/// Upload and delete the differences to the servers of `accounts` for as
/// long as the surface lives.
pub(crate) fn drain(cx: &SurfaceCx<Calendar>, accounts: &DeviceAccounts) {
    let held = Rc::clone(accounts);
    cx.drain(
        move |state: &Calendar| uploads(state, &held.borrow()),
        |job: UploadJob, cx: &mut EffectCx<Calendar>| {
            let holds = fingerprint(&job.event);
            Ok(cx.spawn(
                move |_| async move { caldav::push_jobs(std::slice::from_ref(&job)).await },
                move |(), result: MutationResult, cx| {
                    for revision in &result.revisions {
                        if result.confirmed.contains(&revision.id) {
                            cx.invoke(Calendar::SERVER_APPLIED.with(calendar::ServerApplied {
                                event: revision.id,
                                at: RemoteAt {
                                    calendar: revision.calendar_id.clone(),
                                    href: revision.href.clone(),
                                    etag: revision.etag.clone(),
                                },
                                holds: holds.clone(),
                            }));
                        }
                    }
                    outcome(&result)
                },
            ))
        },
    );
    let held = Rc::clone(accounts);
    cx.drain(
        move |state: &Calendar| deletes(state, &held.borrow()),
        |job: RemoveJob, cx: &mut EffectCx<Calendar>| {
            Ok(cx.spawn(
                move |_| async move { caldav::remove_jobs(std::slice::from_ref(&job)).await },
                |(), result: MutationResult, cx| {
                    for id in &result.confirmed {
                        cx.invoke(
                            Calendar::SERVER_REMOVED.with(calendar::ServerRemoved { item: *id }),
                        );
                    }
                    outcome(&result)
                },
            ))
        },
    );
}

/// What one upload or delete came to: done, or why it is tried again.
fn outcome(result: &MutationResult) -> EffectAnswer {
    if let Some((_, href)) = result.conflicts.first() {
        return Err(format!(
            "{href} also changed on the server; the local version waits to be uploaded"
        ));
    }
    if let Some((_, reason)) = result.failures.first() {
        return Err(reason.clone());
    }
    Ok(Effected::done())
}

#[cfg(test)]
mod tests {
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
}
