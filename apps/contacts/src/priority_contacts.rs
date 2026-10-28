//! Narrow Contacts → shell priority projection.
//!
//! Only launcher-started Contacts publishes (`TILO_PACKAGE_ID`); tests, shots
//! and standalone binaries stay hermetic. The shell independently verifies
//! the sender's process ancestry before accepting a replacement. Other
//! platforms have no such list, and `ui::notify` says so.

use std::sync::{LazyLock, Mutex};

use ui::notify::PriorityContact;

use crate::resource::Contacts;

const PACKAGE_ID: &str = "so.tilo.contacts";

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var("TILO_PACKAGE_ID").as_deref() == Ok(PACKAGE_ID));

/// The list the shell last accepted. A list it did not accept is published
/// again on the next change instead of becoming a silently stale projection.
static ACCEPTED: Mutex<Vec<PriorityContact>> = Mutex::new(Vec::new());

/// The favourites, sorted by id, as the shell's priority list.
fn projection(store: &Contacts) -> Vec<PriorityContact> {
    let mut projection: Vec<PriorityContact> = store
        .contacts
        .iter()
        .filter(|contact| contact.favorite)
        .map(|contact| PriorityContact {
            id: contact.id.to_string(),
            name: contact.display_name(),
            endpoints: contact
                .emails
                .iter()
                .chain(contact.phones.iter())
                .map(|entry| entry.value.clone())
                .collect(),
        })
        .collect();
    projection.sort_by(|left, right| left.id.cmp(&right.id));
    projection
}

/// Hand the favourites to the shell unless it already accepted exactly
/// them. Never blocks: publishing waits on the session bus, so it runs in
/// the background.
pub(crate) fn publish_if_changed(store: &Contacts) {
    if !*ENABLED {
        return;
    }
    let projection = projection(store);
    if ACCEPTED
        .lock()
        .is_ok_and(|accepted| *accepted == projection)
    {
        return;
    }
    let spawned = ui::background::Background::current().spawn("contacts-priority", move || {
        match ui::notify::publish_priority_contacts(PACKAGE_ID, &projection) {
            Ok(()) => {
                if let Ok(mut accepted) = ACCEPTED.lock() {
                    *accepted = projection;
                }
            }
            Err(error) => tracing::debug!(%error, "the shell did not take the priority contacts"),
        }
    });
    if let Err(error) = spawned {
        tracing::debug!(%error, "could not publish the priority contacts");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_is_disabled_outside_the_verified_package_launch() {
        assert!(!*ENABLED);
    }
}
