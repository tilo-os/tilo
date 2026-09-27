//! The priority-contacts projection an address book publishes to the Tilo
//! shell, which lets those people through Do Not Disturb.
//!
//! With the `priority-contacts` feature on Unix, [`publish`] replaces the
//! shell's list over the session bus's notification service; the shell
//! independently verifies the sender's process ancestry before accepting it.
//! Elsewhere publishing does nothing.

/// One priority contact: stable id, display name, and the email addresses
/// and phone numbers that identify them.
pub type PriorityContact = (String, String, Vec<String>);

/// Whether [`publish`] reaches the shell in this build.
pub const SUPPORTED: bool = cfg!(all(unix, feature = "priority-contacts"));

#[cfg(all(unix, feature = "priority-contacts"))]
mod imp {
    use std::sync::{LazyLock, Mutex, mpsc};

    use super::PriorityContact;

    static LAST: LazyLock<Mutex<Vec<PriorityContact>>> = LazyLock::new(|| Mutex::new(Vec::new()));
    static PUBLISHER: LazyLock<Option<mpsc::SyncSender<Vec<PriorityContact>>>> =
        LazyLock::new(|| {
            let (sender, receiver) = mpsc::sync_channel::<Vec<PriorityContact>>(1);
            #[expect(
                clippy::disallowed_methods,
                reason = "the publisher blocks on the session bus for the process lifetime; \
                          a pool thread would be held forever"
            )]
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
                            proxy.call_method("XTiloReplacePriorityContacts", &(contacts.clone(),));
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

    pub fn publish(projection: Vec<PriorityContact>) {
        let Some(sender) = PUBLISHER.as_ref() else {
            return;
        };
        let Ok(last) = LAST.lock() else { return };
        if *last == projection {
            return;
        }
        let _ = sender.try_send(projection);
    }
}

#[cfg(not(all(unix, feature = "priority-contacts")))]
mod imp {
    use super::PriorityContact;

    pub fn publish(_projection: Vec<PriorityContact>) {}
}

/// Replace the shell's priority contacts with `projection`, sorted by id,
/// unless the shell already accepted exactly this list. Never blocks: the
/// latest list is handed to a background publisher and retried on the next
/// call when the shell did not accept it.
pub fn publish(projection: Vec<PriorityContact>) {
    imp::publish(projection);
}
