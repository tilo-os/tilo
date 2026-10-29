//! The senders whose remote images load without asking: a record Mail
//! keeps beside its own in the workspace it is open in
//! ([`RecordEnv::person`](ui::framework::RecordEnv::person)), synced with
//! the workspace. Trusting a sender is the person's edit, in its history
//! and undone like any of theirs.

use std::collections::BTreeSet;

use ui::framework::PersonRecord;
use ui::prelude::{Model, resource, tilo_resource};
use ui::tilo_resource::Set;

use super::app::MailApp;

/// The senders, by lowercase address, whose remote images load.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct TrustedSenders {
    pub senders: Set<String>,
}

#[resource(kind = "mail.trusted-senders", label = "Trusted senders")]
impl TrustedSenders {
    /// Load remote images from `sender` without asking.
    #[action]
    pub fn trust(&mut self, sender: String) {
        self.senders.insert(sender.to_ascii_lowercase());
    }

    /// Ask again before loading remote images from `sender`.
    #[action]
    pub fn distrust(&mut self, sender: String) {
        self.senders.remove(&sender.to_ascii_lowercase());
    }
}

impl TrustedSenders {
    /// The trusted senders.
    #[must_use]
    pub fn addresses(&self) -> BTreeSet<String> {
        self.senders.iter().cloned().collect()
    }
}

/// The trusted senders record a live window follows, and the set as the
/// window last read or wrote it.
pub(crate) struct Followed {
    record: PersonRecord<TrustedSenders>,
    seen: BTreeSet<String>,
}

impl MailApp {
    /// Follow the person's trusted senders in this window.
    pub(crate) fn follow_trusted_senders(&mut self, record: PersonRecord<TrustedSenders>) {
        let seen = record.with(TrustedSenders::addresses);
        self.shared.borrow_mut().remote_senders.clone_from(&seen);
        self.trusted_senders = Some(Followed { record, seen });
    }

    /// Adopt what another window or device trusted, then write what this
    /// window changed as the person's edits.
    pub(crate) fn sync_trusted_senders(&mut self) {
        let Some(followed) = self.trusted_senders.as_mut() else {
            return;
        };
        let stored = followed.record.with(TrustedSenders::addresses);
        if stored != followed.seen {
            self.shared.borrow_mut().remote_senders.clone_from(&stored);
            followed.seen = stored;
            self.shared.borrow_mut().dirty = true;
            return;
        }
        let shown = self.shared.borrow().remote_senders.clone();
        if shown == followed.seen {
            return;
        }
        let edits = shown
            .difference(&followed.seen)
            .map(|sender| TrustedSenders::TRUST.with(sender.clone()))
            .chain(
                followed
                    .seen
                    .difference(&shown)
                    .map(|sender| TrustedSenders::DISTRUST.with(sender.clone())),
            )
            .collect::<Vec<_>>();
        for edit in &edits {
            if let Err(error) = followed.record.invoke(edit) {
                self.shared.borrow_mut().status =
                    Some(format!("Could not save your trusted senders: {error}"));
                break;
            }
        }
        followed.seen = followed.record.with(TrustedSenders::addresses);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    #[test]
    fn trusting_is_by_address_whatever_its_case() {
        let mut replica = Replica::<TrustedSenders>::new();
        replica
            .apply(TrustedSenders::TRUST.with("Hello@Fieldnotes.Example".to_owned()))
            .unwrap();
        assert_eq!(
            replica.state().unwrap().addresses(),
            BTreeSet::from(["hello@fieldnotes.example".to_owned()])
        );
        replica
            .apply(TrustedSenders::DISTRUST.with("hello@fieldnotes.example".to_owned()))
            .unwrap();
        assert!(replica.state().unwrap().addresses().is_empty());
    }
}
