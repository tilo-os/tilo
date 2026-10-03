//! One logical current-policy projection; destination storage owns its budget.
use super::read::{self, GovernanceEntry, ReadError, RoomRead};
use crate::cached_document::PolicyData;

pub(crate) trait Destination {
    type Error;
    fn owner(&mut self, value: &str) -> Result<(), Self::Error>;
    fn capability(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    fn identity(&mut self, key: &str, bytes: &[u8]) -> Result<(), Self::Error>;
}
pub(crate) fn project<R: RoomRead, D: Destination>(
    read: &R,
    destination: &mut D,
) -> Result<(), ReadError<D::Error>> {
    read::visit_governance_entries(read, |entry| match entry {
        GovernanceEntry::Owner(value) => destination.owner(value),
        GovernanceEntry::Capability { bytes, .. } => destination.capability(bytes),
        GovernanceEntry::Identity { key, bytes } => destination.identity(key, bytes),
        GovernanceEntry::Peer { .. } => Ok(()),
    })
}
impl Destination for PolicyData {
    type Error = std::convert::Infallible;
    fn owner(&mut self, value: &str) -> Result<(), Self::Error> {
        self.owner = Some(value.to_owned());
        Ok(())
    }
    fn capability(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        if let Ok(cap) = serde_json::from_slice(bytes) {
            self.caps.push(cap);
        }
        Ok(())
    }
    fn identity(&mut self, key: &str, bytes: &[u8]) -> Result<(), Self::Error> {
        self.identities.insert(key.to_owned(), bytes.to_vec());
        Ok(())
    }
}
pub(crate) fn legacy(document: &automerge::AutoCommit) -> PolicyData {
    #[cfg(test)]
    super::POLICY_READS.with(|count| count.set(count.get().saturating_add(1)));
    let mut value = PolicyData {
        owner: None,
        caps: Vec::new(),
        identities: std::collections::BTreeMap::new(),
    };
    // This concrete legacy adapter has neither resource failures nor fallible
    // destination operations. Malformed native lookups retain the historical
    // ignored-value behavior inside visit_governance_entries.
    let _ = project(&read::LegacyRead(document), &mut value);
    value
}
