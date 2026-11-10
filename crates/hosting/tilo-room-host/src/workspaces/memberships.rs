//! Which workspaces each person is a member of here, by person, so a person's
//! listing answers with the workspaces they were invited into without reading
//! every workspace this host governs.
//!
//! Every workspace this host governs is loaded at start and its governance is
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
pub(super) struct Memberships {
    /// The workspaces each person is a member of, by their user root.
    by_person: std::sync::RwLock<HashMap<[u8; 32], BTreeSet<String>>>,
    /// The people each workspace lists now, by workspace: what a settle
    /// replaces, so nobody stays a member of one they left.
    by_workspace: std::sync::RwLock<HashMap<String, BTreeSet<[u8; 32]>>>,
}

impl Memberships {
    /// Record that `workspace` lists exactly `people` now, and no one else.
    pub(super) fn record(&self, workspace: &str, people: impl IntoIterator<Item = [u8; 32]>) {
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

    /// Forget `workspace`: this host no longer governs it, so it is in no
    /// one's listing.
    pub(super) fn forget(&self, workspace: &str) {
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
    pub(super) fn holds(&self, person: &[u8; 32], workspace: &str) -> bool {
        self.by_person
            .read_recover()
            .get(person)
            .is_some_and(|workspaces| workspaces.contains(workspace))
    }

    /// The workspaces `person` is a member of, by their ids: whoever answers
    /// them in the order a person reads them sorts them.
    pub(super) fn of(&self, person: &[u8; 32]) -> Vec<String> {
        self.by_person
            .read_recover()
            .get(person)
            .map(|workspaces| workspaces.iter().cloned().collect())
            .unwrap_or_default()
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
