//! The workspace's member list is the one membership its signed governance
//! gives: its owner holds it as a member like any other, a person it declares
//! is a member before any device of theirs joins, and whoever it removes loses
//! both the list and the access.
use super::*;

/// The workspace's row for the person `root`, as its governance lists them.
fn member(peer: &StatePeer, root: [u8; 32], now: SystemTime) -> Option<Member> {
    peer.governance()
        .member_list(now)
        .unwrap()
        .members
        .into_iter()
        .find(|member| member.root() == Some(root))
}

#[test]
fn the_owner_is_a_member_like_any_other() {
    let (peer, _, _, owner_cert, now) = crate::admission_tests::seeded();
    let owner = owner_cert.user_public_key;
    let listed = member(&peer, owner, now).expect("the workspace's owner is one of its members");
    assert_eq!(listed.role, Some(Role::Owner));
    assert!(!listed.removed);
    assert!(listed.administrator);
    let access = peer.governance().member_list_access(now).unwrap();
    assert!(
        access.member_user_roots.contains(&owner),
        "the owner holds the workspace by their root"
    );
    assert!(
        access
            .member_devices
            .contains(&owner_cert.device_public_key),
        "a device of the owner's is a member device"
    );
}

#[test]
fn a_declared_person_is_a_member_before_any_device_joins() {
    let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
    let root = SigningKey::from_bytes(&[71; 32]).verifying_key().to_bytes();
    peer.governance_mut()
        .declare_member(root, Role::Commenter, false, Some("Rosa"), &authority, now)
        .unwrap();
    let listed = member(&peer, root, now).expect("a declared person is a member at once");
    assert_eq!(listed.role, Some(Role::Commenter));
    assert!(!listed.removed);
    assert!(!listed.administrator);
    assert_eq!(
        listed.name, "Rosa",
        "the declaration's name is how they read"
    );
    let access = peer.governance().member_list_access(now).unwrap();
    assert!(
        access.member_user_roots.contains(&root),
        "a key of theirs finds the workspace by their root"
    );
}

#[test]
fn a_removed_member_loses_the_list_and_the_access() {
    let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
    let (guest, person) = crate::admission_tests::add_editor(&mut peer, &authority, now);
    let access = peer.governance().member_list_access(now).unwrap();
    assert!(access.member_user_roots.contains(&person));
    assert!(access.member_devices.contains(&guest.device_public_key));

    peer.governance_mut()
        .remove_member(person, &authority, now)
        .unwrap();
    let listed = member(&peer, person, now).expect("their row stays, so what they did reads");
    assert!(listed.removed);
    let access = peer.governance().member_list_access(now).unwrap();
    assert!(
        !access.member_user_roots.contains(&person),
        "a removed person holds nothing"
    );
    assert!(!access.member_devices.contains(&guest.device_public_key));
}

#[test]
fn a_members_row_counts_the_permissions_given_beyond_their_role() {
    let (mut peer, authority, _, owner_cert, now) = crate::admission_tests::seeded();
    let root = SigningKey::from_bytes(&[72; 32]).verifying_key().to_bytes();
    peer.governance_mut()
        .declare_member(root, Role::Editor, false, Some("Rosa"), &authority, now)
        .unwrap();
    assert_eq!(member(&peer, root, now).unwrap().grants, 0);
    let workspace = peer.workspace_id().to_owned();
    let invite = tilo_action_policy::Grant::new(
        "invite:rosa",
        tilo_action_policy::Grantee::person(hex::encode(root)),
        tilo_action_policy::roles::governance_action(
            &workspace,
            tilo_action_policy::roles::governance::ADD_MEMBER,
        ),
        tilo_action_policy::KindSet::OUTSIDE,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(hex::encode(owner_cert.user_public_key)),
    );
    peer.governance_mut()
        .put_member_grants(&[invite], &authority, now)
        .unwrap();
    // Being an administrator is their role's to say, not one more permission.
    peer.governance_mut()
        .set_member_administrator(root, None, &authority, now)
        .unwrap();
    let listed = member(&peer, root, now).unwrap();
    assert!(listed.administrator);
    assert_eq!(listed.grants, 1, "the one permission beyond their role");
}

#[test]
fn a_device_nobody_named_names_no_person() {
    let (_, _, _, cert, _) = crate::admission_tests::seeded();
    let mut naming = governance::Naming::default();
    naming.learn(
        &cert,
        true,
        Some(tilo_user_identity::UNNAMED_PRIMARY_DEVICE),
    );
    assert_eq!(naming.name(), None, "the stand-in is no name of theirs");
    naming.learn(&cert, true, Some("Ben"));
    assert_eq!(naming.name(), Some("Ben"));
}
