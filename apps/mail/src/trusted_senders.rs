//! The workspace's sender trust, governed by Mail's workspace permissions.

use std::collections::BTreeSet;

use ui::framework::ResourceOpener;

use crate::copies::Converted;
use ui::prelude::{Model, resource, tilo_resource};
use ui::tilo_resource::Set;

pub(crate) use ui::tilo_actions::policy::roles::WORKSPACE_MAIL_TRUST_DOCUMENT as DOCUMENT;

/// The senders, by lowercase address, whose remote images load.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct TrustedSenders {
    pub senders: Set<String>,
    #[model(readonly)]
    pub distrusted: Set<String>,
}

impl TrustedSenders {
    /// What the resource holds: the senders whose remote images load without asking.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "mail.trusted-senders",
    read_content = TrustedSenders::read_document
)]
impl TrustedSenders {
    /// Load remote images from `sender` without asking.
    #[action(writes = senders, writes = distrusted)]
    pub fn trust(&mut self, sender: String) {
        let sender = sender.to_ascii_lowercase();
        self.distrusted.remove(&sender);
        self.senders.insert(sender);
    }

    /// Ask again before loading remote images from `sender`.
    #[action(writes = senders, writes = distrusted)]
    pub fn distrust(&mut self, sender: String) {
        let sender = sender.to_ascii_lowercase();
        self.senders.remove(&sender);
        self.distrusted.insert(sender);
    }

    /// Import previous sender trust
    ///
    /// Keep the workspace's decisions when importing a member's previous list.
    #[action(writes = senders)]
    pub fn import_legacy(&mut self, senders: Vec<String>) {
        for sender in senders {
            let sender = sender.to_ascii_lowercase();
            if !self.distrusted.contains(&sender) {
                self.senders.insert(sender);
            }
        }
    }
}

impl TrustedSenders {
    /// The trusted senders.
    #[must_use]
    pub fn addresses(&self) -> BTreeSet<String> {
        self.senders
            .iter()
            .filter(|sender| !self.distrusted.contains(sender))
            .cloned()
            .collect()
    }
}

/// Take the senders a member trusted before Mail kept one list for the workspace into the
/// workspace's list, keeping what the workspace decided since: a sender distrusted there stays
/// distrusted. The earlier list is only read, and taking it again adds nothing.
pub(crate) fn convert_earlier(opener: &ResourceOpener<TrustedSenders>) -> Converted {
    let Some(target) = opener.open(DOCUMENT) else {
        return Converted::Later;
    };
    if !target.may_read() || !target.may("import-legacy") {
        return Converted::NotGranted;
    }
    let earlier = match opener.legacy_member_state() {
        Ok(Some(earlier)) => earlier,
        Ok(None) => return Converted::Later,
        Err(error) => {
            tracing::warn!(%error, "mail: the earlier trusted senders do not read");
            return Converted::Done;
        }
    };
    let senders: Vec<String> = earlier
        .addresses()
        .into_iter()
        .filter(|sender| {
            target
                .try_get()
                .is_none_or(|now| !now.senders.contains(sender) && !now.distrusted.contains(sender))
        })
        .collect();
    if !senders.is_empty()
        && let Err(error) = target.invoke(&TrustedSenders::IMPORT_LEGACY.with(senders))
    {
        tracing::warn!(%error, "mail: the earlier trusted senders could not be taken in");
        return Converted::Later;
    }
    Converted::Done
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
