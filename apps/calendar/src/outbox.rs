//! The calendar's uploads and deletes for the servers of this device's
//! accounts, drained as its actions queue them.
//!
//! Creating, editing or deleting an event on a remote calendar records the
//! outside consequence in the document (the push and delete queues). Every
//! participant's live window drains those queues to the servers of the
//! accounts its device holds, and acknowledges each confirmed entry with an
//! action, so an entry stays queued — and is tried again when the calendar
//! next changes — until a server confirmed it.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, Effected, SurfaceCx};

use crate::account::AccountStore;
use crate::actions::CalendarAction;
use crate::caldav::{self, MutationResult, RemoveJob, UploadJob};
use crate::model::{Id, Model};
use crate::resource::Calendar;

/// The accounts configured on this device, as the drains read them.
pub(crate) type DeviceAccounts = Rc<RefCell<AccountStore>>;

/// Each queued upload whose calendar an account on this device holds.
pub(crate) fn uploads(state: &Model, accounts: &AccountStore) -> Vec<(Id, UploadJob)> {
    state
        .push_queue
        .iter()
        .filter_map(|id| {
            let event = state.event(*id)?;
            let calendar_id = event.calendar_id.as_ref()?;
            let (account, calendar) = accounts
                .all_calendars()
                .find(|(_, calendar)| &calendar.id == calendar_id)?;
            let binding = state.bindings.get(id);
            Some((
                *id,
                UploadJob {
                    config: account.config.clone(),
                    calendar: calendar.clone(),
                    event: event.clone(),
                    href: binding.map(|(_, href, _)| href.clone()),
                    etag: binding.and_then(|(_, _, etag)| etag.clone()),
                },
            ))
        })
        .collect()
}

/// Each queued delete whose calendar an account on this device holds.
pub(crate) fn deletes(state: &Model, accounts: &AccountStore) -> Vec<(Id, RemoveJob)> {
    state
        .delete_queue
        .iter()
        .filter_map(|(calendar_id, id)| {
            let (account, calendar) = accounts
                .all_calendars()
                .find(|(_, calendar)| &calendar.id == calendar_id)?;
            let binding = state.bindings.get(id);
            Some((
                *id,
                RemoveJob {
                    config: account.config.clone(),
                    calendar: calendar.clone(),
                    id: *id,
                    href: binding.map(|(_, href, _)| href.clone()),
                    etag: binding.and_then(|(_, _, etag)| etag.clone()),
                },
            ))
        })
        .collect()
}

/// Drain both queues to the servers of `accounts` for as long as the
/// surface lives.
pub(crate) fn drain(cx: &SurfaceCx<Calendar>, accounts: &DeviceAccounts) {
    let held = Rc::clone(accounts);
    cx.drain(
        move |state: &Model| uploads(state, &held.borrow()),
        |job: UploadJob, cx: &mut EffectCx<Calendar>| {
            Ok(cx.spawn(
                move |_| async move { caldav::push_jobs(std::slice::from_ref(&job)).await },
                |(), result: MutationResult, cx| {
                    for revision in &result.revisions {
                        cx.apply(&CalendarAction::SetRemoteBinding {
                            id: revision.id.to_string(),
                            calendar_id: revision.calendar_id.clone(),
                            href: revision.href.clone(),
                            etag: revision.etag.clone().unwrap_or_default(),
                        });
                    }
                    for id in &result.confirmed {
                        cx.apply(&CalendarAction::AcknowledgeRemotePush { id: id.to_string() });
                    }
                    outcome(&result)
                },
            ))
        },
    );
    let held = Rc::clone(accounts);
    cx.drain(
        move |state: &Model| deletes(state, &held.borrow()),
        |job: RemoveJob, cx: &mut EffectCx<Calendar>| {
            Ok(cx.spawn(
                move |_| async move { caldav::remove_jobs(std::slice::from_ref(&job)).await },
                |(), result: MutationResult, cx| {
                    for id in &result.confirmed {
                        cx.apply(&CalendarAction::AcknowledgeRemoteDelete { id: id.to_string() });
                    }
                    outcome(&result)
                },
            ))
        },
    );
}

/// What one upload or delete came to: done, or why the entry stays queued.
fn outcome(result: &MutationResult) -> EffectAnswer {
    if let Some((_, href)) = result.conflicts.first() {
        return Err(format!(
            "{href} also changed on the server; the local version stays queued"
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
    use crate::actions::{CalendarAction, EventData};
    use tilo_actions::Action as _;

    /// Only what an account on this device can reach is drained here; the
    /// rest stays queued for a device that holds its account.
    #[test]
    fn the_queues_drain_to_the_calendars_this_device_holds() {
        let accounts = AccountStore::sample();
        let (_, calendar) = accounts.all_calendars().next().expect("a sample calendar");
        let mut doc = automerge::AutoCommit::new();
        let mut held = crate::model::seed_demo(chrono::Local::now().date_naive()).events[0].clone();
        held.calendar_id = Some(calendar.id.clone());
        held.project = None;
        let mut elsewhere = held.clone();
        elsewhere.id = uuid::Uuid::new_v4();
        elsewhere.calendar_id = Some("another-device/calendar".into());
        for event in [&held, &elsewhere] {
            CalendarAction::UpsertEvent(EventData::from_event(event))
                .apply(&mut doc)
                .expect("create");
        }
        let state = crate::doc::CalDoc::read_model(&doc);
        assert_eq!(state.push_queue.len(), 2, "both creations are queued");
        let jobs = uploads(&state, &accounts);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].0, held.id);
        assert_eq!(jobs[0].1.calendar.id, calendar.id);

        CalendarAction::DeleteEvent {
            id: held.id.to_string(),
            remote_delete: Some(calendar.id.clone()),
        }
        .apply(&mut doc)
        .expect("delete");
        let state = crate::doc::CalDoc::read_model(&doc);
        assert!(
            uploads(&state, &accounts).is_empty(),
            "a deleted event is not uploaded"
        );
        let jobs = deletes(&state, &accounts);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].0, held.id);

        CalendarAction::AcknowledgeRemoteDelete {
            id: held.id.to_string(),
        }
        .apply(&mut doc)
        .expect("acknowledge");
        let state = crate::doc::CalDoc::read_model(&doc);
        assert!(
            deletes(&state, &accounts).is_empty(),
            "a confirmed delete leaves the queue"
        );
    }
}
