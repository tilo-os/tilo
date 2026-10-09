//! Telling the servers of this device's accounts what changed.
//!
//! Each remote item carries what its server last confirmed holding
//! ([`Calendar::remote`]). After every change — a click, the palette, an
//! agent, a collaborator, an undo — the calendar queues the upload of each
//! remote event its server does not hold as it is, and the delete of each
//! copy a server holds of an event gone from that calendar here
//! ([`Calendar::uploads`], [`Calendar::deletes`]). The device holding the
//! account carries each entry out, records what the server confirmed
//! (`server-applied`, `server-removed`) and settles it; one that fails stays
//! pending and is tried again when the calendar next changes.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, Effected, SurfaceCx};

use crate::account::AccountStore;
use crate::caldav::{self, MutationResult, RemoveJob, UploadJob};
use crate::model::{Calendar, Id, RemoteCopy};
use crate::resource::{RemoteAt, calendar, fingerprint};

/// The accounts configured on this device, as the drains read them.
pub(crate) type DeviceAccounts = Rc<RefCell<AccountStore>>;

/// Whether an account on this device holds the remote calendar `id`.
fn holds(accounts: &AccountStore, id: &str) -> bool {
    accounts
        .all_calendars()
        .any(|(_, calendar)| calendar.id == id)
}

/// The upload of `event` to the calendar `calendar_id`, as this device
/// sends it: the event as it is now, guarded by the version the server
/// last confirmed.
fn upload_job(
    state: &Calendar,
    accounts: &AccountStore,
    event: Id,
    calendar_id: &str,
) -> Option<UploadJob> {
    let event = state.event(event)?;
    if event.calendar_id.as_deref() != Some(calendar_id) {
        return None;
    }
    let (account, calendar) = accounts
        .all_calendars()
        .find(|(_, calendar)| calendar.id == calendar_id)?;
    let held = state
        .remote
        .get(&event.id.to_string())
        .filter(|copy| copy.calendar == calendar_id);
    Some(UploadJob {
        config: account.config.clone(),
        calendar: calendar.clone(),
        event: event.clone(),
        href: held
            .map(|copy| copy.href.clone())
            .filter(|href| !href.is_empty()),
        etag: held.and_then(|copy| copy.etag.clone()),
    })
}

/// Each queued upload this device carries out.
#[cfg(test)]
pub(crate) fn uploads(state: &Calendar, accounts: &AccountStore) -> Vec<(Id, UploadJob)> {
    state
        .uploads
        .pending()
        .filter_map(|(_, upload)| {
            upload_job(state, accounts, upload.event, &upload.calendar)
                .map(|job| (upload.event, job))
        })
        .collect()
}

/// Each queued delete this device carries out.
#[cfg(test)]
pub(crate) fn deletes(state: &Calendar, accounts: &AccountStore) -> Vec<(Id, RemoveJob)> {
    state
        .deletes
        .pending()
        .filter_map(|(_, removal)| {
            let (account, calendar) = accounts
                .all_calendars()
                .find(|(_, calendar)| calendar.id == removal.calendar)?;
            Some((
                removal.item,
                RemoveJob {
                    config: account.config.clone(),
                    calendar: calendar.clone(),
                    id: removal.item,
                    href: Some(removal.href.clone()).filter(|href| !href.is_empty()),
                    etag: removal.etag.clone(),
                },
            ))
        })
        .collect()
}

/// The copies on servers an upload waits to replace: what protects the
/// local version from what the server sends meanwhile.
pub(crate) fn pending_copies(state: &Calendar) -> Vec<&RemoteCopy> {
    state
        .uploads
        .pending()
        .filter_map(|(_, upload)| {
            state
                .remote
                .get(&upload.event.to_string())
                .filter(|copy| copy.calendar == upload.calendar)
        })
        .collect()
}

/// Carry out the queued uploads and deletes of the calendars `accounts`
/// holds for as long as the surface lives.
pub(crate) fn drain(cx: &SurfaceCx<Calendar>, accounts: &DeviceAccounts) {
    let owner = Rc::clone(accounts);
    let held = Rc::clone(accounts);
    cx.outbox(
        Calendar::paths().uploads,
        move |_, upload| holds(&owner.borrow(), &upload.calendar),
        move |_, upload, cx: &mut EffectCx<Calendar>| {
            let state = cx.live().get();
            let Some(job) = upload_job(&state, &held.borrow(), upload.event, &upload.calendar)
            else {
                // Gone, or moved off that calendar since: nothing to send.
                return Ok(Effected::done());
            };
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
    let owner = Rc::clone(accounts);
    let held = Rc::clone(accounts);
    cx.outbox(
        Calendar::paths().deletes,
        move |_, removal| holds(&owner.borrow(), &removal.calendar),
        move |_, removal, cx: &mut EffectCx<Calendar>| {
            let Some((account, calendar)) = held
                .borrow()
                .all_calendars()
                .find(|(_, calendar)| calendar.id == removal.calendar)
                .map(|(account, calendar)| (account.config.clone(), calendar.clone()))
            else {
                return Err(format!("no account here holds {}", removal.calendar));
            };
            let job = RemoveJob {
                config: account,
                calendar,
                id: removal.item,
                href: Some(removal.href).filter(|href| !href.is_empty()),
                etag: removal.etag,
            };
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
mod tests;
