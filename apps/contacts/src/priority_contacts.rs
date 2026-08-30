//! Narrow Contacts → shell priority projection.
//!
//! Only launcher-started Contacts enables this publisher (`QOS_PACKAGE_ID`);
//! tests, shots and standalone binaries stay hermetic. The shell independently
//! verifies the D-Bus sender's process ancestry before accepting a replacement.

use std::sync::{LazyLock, Mutex, mpsc};

use crate::model::ContactStore;

type WireContact = (String, String, Vec<String>);

static LAST: LazyLock<Mutex<Vec<WireContact>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static PUBLISHER: LazyLock<Option<mpsc::SyncSender<Vec<WireContact>>>> = LazyLock::new(|| {
    if std::env::var("QOS_PACKAGE_ID").as_deref() != Ok("dev.qos.contacts") {
        return None;
    }
    let (sender, receiver) = mpsc::sync_channel::<Vec<WireContact>>(1);
    let _ = std::thread::Builder::new()
        .name("contacts-priority-publisher".into())
        .spawn(move || {
            while let Ok(contacts) = receiver.recv() {
                let Ok(connection) = zbus::blocking::Connection::session() else {
                    continue;
                };
                let Ok(proxy) = zbus::blocking::Proxy::new(
                    &connection,
                    "org.freedesktop.Notifications",
                    "/org/freedesktop/Notifications",
                    "org.freedesktop.Notifications",
                ) else {
                    continue;
                };
                let published: Result<zbus::Message, _> =
                    proxy.call_method("XQosReplacePriorityContacts", &(contacts.clone(),));
                // Only acknowledge after the shell accepted the replacement.
                // A missing notification service or an attribution failure is
                // retried on the next app sync instead of becoming a silently
                // stale projection for the rest of the session.
                if published.is_ok()
                    && let Ok(mut last) = LAST.lock()
                {
                    *last = contacts;
                }
            }
        });
    Some(sender)
});

pub(crate) fn publish_if_changed(store: &ContactStore) {
    let Some(sender) = PUBLISHER.as_ref() else {
        return;
    };
    let mut projection: Vec<WireContact> = store
        .contacts
        .iter()
        .filter(|contact| contact.favorite)
        .map(|contact| {
            let endpoints = contact
                .emails
                .iter()
                .chain(contact.phones.iter())
                .map(|entry| entry.value.clone())
                .collect();
            (contact.id.to_string(), contact.display_name(), endpoints)
        })
        .collect();
    projection.sort_by(|left, right| left.0.cmp(&right.0));
    let Ok(last) = LAST.lock() else { return };
    if *last == projection {
        return;
    }
    let _ = sender.try_send(projection);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_is_disabled_outside_the_verified_package_launch() {
        assert!(PUBLISHER.is_none());
    }
}
