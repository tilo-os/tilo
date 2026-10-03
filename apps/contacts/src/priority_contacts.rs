//! Narrow Contacts → shell priority projection.
//!
//! Only launcher-started Contacts enables this publisher (`TILO_PACKAGE_ID`);
//! tests, shots and standalone binaries stay hermetic. The shell independently
//! verifies the sender's process ancestry before accepting a replacement.
//! Other platforms have no such list, and `ui::notify` says so.

use std::sync::LazyLock;

use crate::model::ContactStore;

static ENABLED: LazyLock<bool> =
    LazyLock::new(|| std::env::var("TILO_PACKAGE_ID").as_deref() == Ok("so.tilo.contacts"));

pub(crate) fn publish_if_changed(store: &ContactStore) {
    if !ui::priority_contacts::SUPPORTED || !*ENABLED {
        return;
    }
    let mut projection: Vec<ui::priority_contacts::PriorityContact> = store
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
    ui::priority_contacts::publish(projection);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_is_disabled_outside_the_verified_package_launch() {
        assert!(!*ENABLED);
    }
}
