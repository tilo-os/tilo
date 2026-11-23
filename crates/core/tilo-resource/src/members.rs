//! Who a workspace's members are, as the host running a record's code knows them: what an action
//! binds a typed `@Name` to and what a reading of the record calls an author by.
//!
//! The record keeps member ids only. Names are the workspace's member records, which the host
//! reads (a window from its member list, an agent from the workspace's governance) and lends here
//! before the record's code runs. Whatever an action derives from them it stores, so a replica
//! replaying the change reads the stored result and never this list.

use std::cell::RefCell;
use std::collections::BTreeMap;

use tilo_action_policy::MemberId;

thread_local! {
    static MEMBERS: RefCell<BTreeMap<MemberId, String>> = const { RefCell::new(BTreeMap::new()) };
}

/// Take `members`, each with what the workspace calls them, as the members the record's code
/// reads from now on; those the workspace removed among them. A member nobody named is kept,
/// with an empty name: a view shows them by the same name the rest of the window shows someone
/// unnamed, and a picker leaves them out ([`named`]).
pub fn know(members: impl IntoIterator<Item = (MemberId, String)>) {
    let members: BTreeMap<MemberId, String> = members
        .into_iter()
        .map(|(id, name)| (id, name.trim().to_owned()))
        .collect();
    MEMBERS.with_borrow_mut(|held| *held = members);
}

/// What the workspace calls `member`: its name, empty for one nobody named; `None` for one the
/// workspace does not list at all.
#[must_use]
pub fn name_of(member: &MemberId) -> Option<String> {
    MEMBERS.with_borrow(|held| held.get(member).cloned())
}

/// Every member the workspace lists, named or not, with the name it gives them, in id order.
#[must_use]
pub fn listed() -> Vec<(MemberId, String)> {
    MEMBERS.with_borrow(|held| {
        held.iter()
            .map(|(id, name)| (id.clone(), name.clone()))
            .collect()
    })
}

/// Every member the workspace names, with their name, in id order: those it lists unnamed are
/// left out.
#[must_use]
pub fn named() -> Vec<(MemberId, String)> {
    MEMBERS.with_borrow(|held| {
        held.iter()
            .filter(|(_, name)| !name.is_empty())
            .map(|(id, name)| (id.clone(), name.clone()))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_is_named_as_the_host_last_said() {
        let ada = MemberId::person("ad".repeat(32));
        assert_eq!(name_of(&ada), None);
        know([
            (ada.clone(), " Ada ".to_owned()),
            (MemberId::agent("x"), " ".to_owned()),
        ]);
        assert_eq!(name_of(&ada).as_deref(), Some("Ada"));
        assert_eq!(
            name_of(&MemberId::agent("x")).as_deref(),
            Some(""),
            "a member nobody named is listed with no name"
        );
        assert_eq!(named().len(), 1, "a picker offers the named alone");
        assert_eq!(listed().len(), 2, "a view knows every member there is");
        know([]);
        assert_eq!(name_of(&ada), None);
    }
}
