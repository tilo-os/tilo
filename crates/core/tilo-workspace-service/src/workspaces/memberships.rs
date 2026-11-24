//! Which workspaces each person is a member of here, by person, so a person's
//! listing answers with the workspaces they were invited into, and the
//! handshake trusts every member, without reading every workspace this host
//! serves.
//!
//! Every workspace this host serves is loaded at start and its member list is
//! settled after each decision ([`super::Workspaces::settle`]), so the index
//! is filled as the host opens its workspaces and replaced for a workspace
//! whenever its membership moves. Nothing is kept on disk: the workspaces'
//! own records are, and the index is derived from them at every start.
//!
//! A workspace of a person's own is kept by [`super::own`], which is where a
//! listing finds those; this index answers the rest — the workspaces they
//! were invited into, or were made a member of.

use std::collections::{BTreeSet, HashMap};

use tilo_lock::RwLockRecover as _;

/// The people in the workspaces here, by person and by workspace.
#[derive(Default)]
pub(crate) struct Memberships {
    /// The workspaces each person is a member of, by their user root.
    by_person: std::sync::RwLock<HashMap<[u8; 32], BTreeSet<String>>>,
    /// The people each workspace lists now, by workspace: what a settle
    /// replaces, so nobody stays a member of one they left.
    by_workspace: std::sync::RwLock<HashMap<String, BTreeSet<[u8; 32]>>>,
    /// The devices each workspace here names among its holders, by workspace: a
    /// holder is no member and no trusted person, and the handshake admits the
    /// one device a naming names, to keep and carry the workspace.
    holders_by_workspace: std::sync::RwLock<HashMap<String, BTreeSet<[u8; 32]>>>,
}

impl Memberships {
    /// Record that `workspace` lists exactly `people` now, and no one else.
    pub(crate) fn record(&self, workspace: &str, people: impl IntoIterator<Item = [u8; 32]>) {
        let people: BTreeSet<[u8; 32]> = people.into_iter().collect();
        let before = self
            .by_workspace
            .write_recover()
            .insert(workspace.to_owned(), people.clone());
        let left: BTreeSet<[u8; 32]> = before
            .unwrap_or_default()
            .difference(&people)
            .copied()
            .collect();
        if !left.is_empty() {
            let mut by_person = self.by_person.write_recover();
            for person in left {
                if let Some(workspaces) = by_person.get_mut(&person) {
                    workspaces.remove(workspace);
                    if workspaces.is_empty() {
                        by_person.remove(&person);
                    }
                }
            }
        }
        let joined: Vec<[u8; 32]> = people.iter().copied().collect();
        if !joined.is_empty() {
            let mut by_person = self.by_person.write_recover();
            for person in joined {
                by_person
                    .entry(person)
                    .or_default()
                    .insert(workspace.to_owned());
            }
        }
    }

    /// Record that `workspace` names exactly the devices `devices` among its
    /// holders now.
    pub(crate) fn record_holders(
        &self,
        workspace: &str,
        devices: impl IntoIterator<Item = [u8; 32]>,
    ) {
        self.holders_by_workspace
            .write_recover()
            .insert(workspace.to_owned(), devices.into_iter().collect());
    }

    /// Every device a workspace here names among its holders now.
    pub(crate) fn holder_devices(&self) -> Vec<[u8; 32]> {
        self.holders_by_workspace
            .read_recover()
            .values()
            .flatten()
            .copied()
            .collect()
    }

    /// Forget `workspace`: this host serves it no more, so it is in no one's
    /// listing.
    pub(crate) fn forget(&self, workspace: &str) {
        self.holders_by_workspace
            .write_recover()
            .remove(workspace);
        let Some(people) = self.by_workspace.write_recover().remove(workspace) else {
            return;
        };
        let mut by_person = self.by_person.write_recover();
        for person in people {
            if let Some(workspaces) = by_person.get_mut(&person) {
                workspaces.remove(workspace);
                if workspaces.is_empty() {
                    by_person.remove(&person);
                }
            }
        }
    }

    /// Whether `person` is a member of `workspace` now.
    pub(crate) fn holds(&self, person: &[u8; 32], workspace: &str) -> bool {
        self.by_person
            .read_recover()
            .get(person)
            .is_some_and(|workspaces| workspaces.contains(workspace))
    }

    /// Every person a workspace here has as a member now.
    pub(crate) fn people(&self) -> Vec<[u8; 32]> {
        self.by_person.read_recover().keys().copied().collect()
    }

    /// Whether `person` is a member of any workspace here now.
    pub(crate) fn holds_any(&self, person: &[u8; 32]) -> bool {
        self.by_person.read_recover().contains_key(person)
    }

    /// The workspaces `person` is a member of, by their ids: whoever answers
    /// them in the order a person reads them sorts them.
    pub(crate) fn of(&self, person: &[u8; 32]) -> Vec<String> {
        self.by_person
            .read_recover()
            .get(person)
            .map(|workspaces| workspaces.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl super::Workspaces {
    /// Whether the host knows the person `certificate` certifies a device of, to answer its own
    /// operations for: one its operator's trust names (its own person, the people it lists, or
    /// anyone), or a member of a workspace held here. A device the operator blocks is known to
    /// nobody.
    pub(crate) fn knows_at_host(&self, certificate: &tilo_user_identity::DeviceCert) -> bool {
        let host = &self.inner.host;
        let person = certificate.user_public_key;
        let blocked = host.blocked.read_recover();
        if blocked.blocks(&certificate.device_public_key, &person) || blocked.revokes(certificate) {
            return false;
        }
        drop(blocked);
        host.trust_policy
            .read_recover()
            .admits(certificate, &host.cert.user_public_key)
            || self.inner.memberships.holds_any(&person)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_lists_exactly_the_people_its_settle_named() {
        let memberships = Memberships::default();
        let ada = [1; 32];
        let rosa = [2; 32];
        memberships.record("w", [ada, rosa]);
        assert_eq!(memberships.of(&ada), ["w"]);
        assert!(memberships.holds(&rosa, "w"));
        // Rosa left: she is in no listing, and Ada still is.
        memberships.record("w", [ada]);
        assert_eq!(memberships.of(&rosa), Vec::<String>::new());
        assert!(memberships.holds(&ada, "w"));
        // The workspace is released: nobody is in it.
        memberships.forget("w");
        assert!(!memberships.holds(&ada, "w"));
        assert_eq!(memberships.of(&ada), Vec::<String>::new());
    }
}
