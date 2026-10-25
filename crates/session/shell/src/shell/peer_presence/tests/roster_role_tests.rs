//! A roster row's role chip is the room's role for the person it vouches
//! for at the session, never one read off what the session is called.
use super::*;

/// A workspace `owner` owns, whose room admits `member` as `role` —
/// the membership invite redemption writes, keyed by the person's key
/// hex.
fn room_admitting(
    owner: &str,
    member: &str,
    role: crate::shell::state::CapRole,
) -> crate::shell::state::Workspace {
    use crate::shell::state::{Workspace, WorkspaceId};
    let mut ws = Workspace::in_room(
        WorkspaceId(1),
        crate::shell::state::RoomId::new(),
        "Design review",
    );
    ws.set_owner(owner.to_string());
    ws.govern_as_at(
        owner,
        tilo_action_policy::Change::AddMember {
            person: member.to_owned(),
            role,
        },
        tilo_clock::unix_now(),
    )
    .expect("the owner admits the member");
    ws
}

#[test]
fn room_roster_takes_roles_from_the_certified_principal_not_the_label() {
    use crate::shell::state::{CapRole, role_id};
    let bob = crate::shell::state::person_id_of([0xb0; 32]).expect("a person key");
    let handle = 7_u64;
    let arrived = std::collections::BTreeSet::from([handle]);
    let none = std::collections::BTreeMap::new();

    // Phase `an_invited_member_shows_their_policy_role`. Bob was
    // invited as an editor; the room calls him "Bob" and his
    // certified device belongs to his key. The chip is the policy's.
    {
        let ws = room_admitting("ada", &bob, CapRole::Editor);
        let labels = PeerLabels::from([(handle, "Bob".to_string())]);
        let principals = std::collections::BTreeMap::from([(handle, bob.clone())]);
        let rows = room_roster(&ws, &labels, &arrived, 1, &principals, &none);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Bob", "the label is still what he is called");
        assert_eq!(
            rows[0].role_id.as_deref(),
            Some(role_id(CapRole::Editor)),
            "an invited editor's row carries the role the Members tab shows",
        );
    }

    // Phase `a_label_spelling_somebody_else_s_id_does_not_borrow_their_role`.
    // A certified device is asked about by its person only: a label
    // that happens to read as the owner's id lends it nothing.
    {
        let ws = room_admitting("ada", &bob, CapRole::Viewer);
        let labels = PeerLabels::from([(handle, "ada".to_string())]);
        let principals = std::collections::BTreeMap::from([(handle, bob.clone())]);
        let rows = room_roster(&ws, &labels, &arrived, 1, &principals, &none);
        assert_eq!(
            rows[0].role_id.as_deref(),
            Some(role_id(CapRole::Viewer)),
            "the chip is the certified person's, not whoever the label spells",
        );
    }

    // Phase `the_owner_endpoint_named_with_the_user_id_keeps_owner_role`.
    // The inviter labels its own endpoint with its user id; the room
    // vouches for that endpoint as its owner, so it keeps the owner chip.
    {
        let ws = room_admitting("ada", &bob, CapRole::Editor);
        let labels = PeerLabels::from([(handle, "ada".to_string())]);
        let principals = std::collections::BTreeMap::from([(handle, "ada".to_string())]);
        let rows = room_roster(&ws, &labels, &arrived, 1, &principals, &none);
        assert_eq!(
            rows[0].role_id.as_deref(),
            Some(role_id(CapRole::Owner)),
            "the endpoint the room vouches for as its owner wears the owner chip",
        );
    }

    // Phase `a_label_alone_lends_no_role`. A session the room vouches
    // for nobody at, called by the owner's id, is still nobody.
    {
        let ws = room_admitting("ada", &bob, CapRole::Editor);
        let labels = PeerLabels::from([(handle, "ada".to_string())]);
        let rows = room_roster(
            &ws,
            &labels,
            &arrived,
            1,
            &std::collections::BTreeMap::new(),
            &none,
        );
        assert_eq!(rows[0].name, "ada", "it is still called what it says");
        assert_eq!(
            rows[0].role_id, None,
            "a name, whoever it spells, is no role",
        );
    }

    {
        let ws = room_admitting("ada", "someone-else", CapRole::Editor);
        let labels = PeerLabels::from([(handle, "Bob".to_string())]);
        let principals = std::collections::BTreeMap::from([(handle, bob)]);
        let rows = room_roster(&ws, &labels, &arrived, 1, &principals, &none);
        assert_eq!(
            rows[0].role_id, None,
            "a certified person the room never admitted has no role to show",
        );
    }

    // Phase `another_device_of_this_person_is_that_device_not_someone`.
    {
        let ws = room_admitting("ada", "bob", CapRole::Editor);
        let labels = PeerLabels::from([(handle, "Ada".to_string())]);
        let own = std::collections::BTreeMap::from([(handle, "Orange Pi".to_string())]);
        let rows = room_roster(&ws, &labels, &arrived, 1, &none, &own);
        assert!(rows[0].own_device, "the row is one of the person's devices");
        assert_eq!(rows[0].name, "Orange Pi", "named by the device");
    }
}
