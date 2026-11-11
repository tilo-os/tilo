//! One logical current-policy projection; destination storage owns its budget.
use super::read::{self, GovernanceEntry, ReadError, WorkspaceRead};
use crate::cached_document::PolicyData;

pub(crate) trait Destination {
    type Error;
    fn owner(&mut self, value: &str) -> Result<(), Self::Error>;
    fn grant(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    fn member(&mut self, key: &str, bytes: &[u8]) -> Result<(), Self::Error>;
    /// When a member was admitted; only a destination that shows people
    /// keeps it.
    fn member_joined(&mut self, _key: &str, _bytes: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }
    fn identity(&mut self, key: &str, bytes: &[u8]) -> Result<(), Self::Error>;
    /// The display name beside a device row; only a destination that shows
    /// people keeps it.
    fn peer(&mut self, _key: &str, _name: &str) -> Result<(), Self::Error> {
        Ok(())
    }
}
pub(crate) fn project<R: WorkspaceRead, D: Destination>(
    read: &R,
    destination: &mut D,
) -> Result<(), ReadError<D::Error>> {
    read::visit_governance_entries(read, |entry| match entry {
        GovernanceEntry::Owner(value) => destination.owner(value),
        GovernanceEntry::Grant { bytes, .. } => destination.grant(bytes),
        GovernanceEntry::Member { key, bytes } => destination.member(key, bytes),
        GovernanceEntry::MemberJoined { key, bytes } => destination.member_joined(key, bytes),
        GovernanceEntry::Identity { key, bytes } => destination.identity(key, bytes),
        GovernanceEntry::Peer { key, name } => destination.peer(key, name),
        GovernanceEntry::Sandbox { .. } => Ok(()),
    })
}
/// Rows of a workspace's governance that did not decode: their grants and
/// roles are missing from the policy projected beside them, so a person whose
/// access rests on one is told their access could not be read rather than that
/// they are a stranger.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Damaged {
    /// Grant rows that did not decode, whichever node they were for: a grant
    /// the workspace was given is missing from this policy.
    pub grants: usize,
    /// People whose member row did not decode: they hold no role here.
    pub members: std::collections::BTreeSet<String>,
}

impl Damaged {
    /// Whether a person with no role may owe that to a row that did not read
    /// back, rather than to holding none.
    pub(crate) fn hides(&self, person: &str) -> bool {
        self.grants > 0 || self.members.contains(person)
    }
}

impl Destination for PolicyData {
    type Error = std::convert::Infallible;
    fn owner(&mut self, value: &str) -> Result<(), Self::Error> {
        self.owner = Some(value.to_owned());
        Ok(())
    }
    fn grant(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        match serde_json::from_slice(bytes) {
            Ok(grant) => self.grants.push(grant),
            // A row that does not read back grants nobody, so the person who
            // was given it finds the workspace shut and this says why.
            Err(error) => {
                self.damaged.grants = self.damaged.grants.saturating_add(1);
                tracing::warn!(%error, "a workspace grant row does not decode; it grants nobody");
            }
        }
        Ok(())
    }
    fn member(&mut self, key: &str, bytes: &[u8]) -> Result<(), Self::Error> {
        match serde_json::from_slice(bytes) {
            Ok(role) => {
                self.members.insert(key.to_owned(), role);
            }
            // A member whose role does not read back holds none; their writes
            // are refused with this recorded, so they learn their access could
            // not be read instead of that they are no member.
            Err(error) => {
                self.damaged.members.insert(key.to_owned());
                tracing::warn!(%error, %key, "a member row does not decode; the person holds no role");
            }
        }
        Ok(())
    }
    fn member_joined(&mut self, key: &str, bytes: &[u8]) -> Result<(), Self::Error> {
        match serde_json::from_slice(bytes) {
            Ok(at) => {
                self.joined.insert(key.to_owned(), at);
            }
            Err(error) => {
                tracing::warn!(%error, %key, "a joined row does not decode; the join is undated");
            }
        }
        Ok(())
    }
    fn identity(&mut self, key: &str, bytes: &[u8]) -> Result<(), Self::Error> {
        self.identities.insert(key.to_owned(), bytes.to_vec());
        Ok(())
    }
    fn peer(&mut self, key: &str, name: &str) -> Result<(), Self::Error> {
        self.names.insert(key.to_owned(), name.to_owned());
        Ok(())
    }
}
pub(crate) fn legacy(document: &automerge::AutoCommit) -> PolicyData {
    #[cfg(test)]
    super::POLICY_READS.with(|count| count.set(count.get().saturating_add(1)));
    let mut value = PolicyData {
        owner: None,
        members: std::collections::BTreeMap::new(),
        joined: std::collections::BTreeMap::new(),
        grants: Vec::new(),
        identities: std::collections::BTreeMap::new(),
        names: std::collections::BTreeMap::new(),
        damaged: Damaged::default(),
    };
    // This concrete legacy adapter has neither resource failures nor fallible
    // destination operations. Malformed native lookups retain the historical
    // ignored-value behavior inside visit_governance_entries.
    // ignored-failure: the legacy adapter's destination operations are infallible
    let _ = project(&read::LegacyRead(document), &mut value);
    value
}

#[cfg(test)]
mod tests {
    use super::{Damaged, Destination};
    use crate::cached_document::PolicyData;

    fn empty() -> PolicyData {
        PolicyData {
            owner: None,
            members: std::collections::BTreeMap::new(),
            joined: std::collections::BTreeMap::new(),
            grants: Vec::new(),
            identities: std::collections::BTreeMap::new(),
            names: std::collections::BTreeMap::new(),
            damaged: Damaged::default(),
        }
    }

    /// Rows a person's access rests on that do not read back are recorded, not
    /// only warned about: the member whose role does not decode, and a grant
    /// that does not decode, which could have been anyone's.
    #[test]
    fn rows_that_do_not_read_back_are_recorded() {
        let mut policy = empty();
        assert!(
            !policy.damaged.hides("them"),
            "a policy whose rows read back hides nothing"
        );
        policy
            .member("them", b"not a role")
            .expect("the projection is infallible");
        assert!(policy.damaged.members.contains("them"));
        assert!(policy.damaged.hides("them"), "their role did not read back");
        assert!(
            !policy.damaged.hides("someone else"),
            "another person's row read back"
        );
        policy
            .grant(b"not a grant")
            .expect("the projection is infallible");
        assert_eq!(policy.damaged.grants, 1);
        assert!(
            policy.damaged.hides("someone else"),
            "a grant that does not decode could have been theirs"
        );
    }
}
