//! A room's administrators govern it: the governance writer gives them a
//! grant the Editor's own rule on the room's resource does not outrank, and
//! rewrites the grant rooms held before it so their administrators govern
//! too.
use super::*;
use tilo_action_policy::KindSet;
use tilo_action_policy::roles::{self, governance};

/// How many of the room's governance actions the member with device
/// `device` may run, as `peer`'s room policy decides it now.
fn governs(peer: &StatePeer, device: [u8; 32], now: SystemTime) -> usize {
    let member = peer.permissions(device, now).unwrap();
    let bound = roles::foreign_device_bound();
    let authority = member.authority(&bound);
    let cx = tilo_action_policy::Context::at_time(now);
    governance::ALL
        .into_iter()
        .filter(|action| {
            member
                .policy
                .check_action(&authority, action, &cx, false)
                .is_ok()
        })
        .count()
}

const EVERY: usize = governance::ALL.len();

fn is_administrator(peer: &StatePeer, user: [u8; 32], now: SystemTime) -> bool {
    peer.governance()
        .governed_members(now)
        .unwrap()
        .iter()
        .any(|member| member.user_public_key == user && member.administrator)
}

#[test]
fn an_administrator_governs_and_an_editor_does_not() {
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    assert_eq!(governs(&peer, editor.device_public_key, now), 0);

    assert!(
        peer.governance_mut()
            .set_member_administrator(user, &host_key, now)
            .unwrap()
    );
    assert!(is_administrator(&peer, user, now));
    assert_eq!(governs(&peer, editor.device_public_key, now), EVERY);
    let held = peer.permissions(editor.device_public_key, now).unwrap();
    assert_eq!(held.policy.role_of(&held.person), Some(Role::Editor));

    // Setting a role takes the standing back with the grant.
    peer.governance_mut()
        .set_member_role(user, crate::join::MemberRole::Editor, &host_key, now)
        .unwrap();
    assert!(!is_administrator(&peer, user, now));
    assert_eq!(governs(&peer, editor.device_public_key, now), 0);
}

#[test]
fn an_administrator_grant_on_the_rooms_resource_is_rewritten_once() {
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let person = hex::encode(user);
    let owner = Subject::Person(hex::encode(peer.authority().user_public_key));
    // The grant as rooms held it before: on the room's resource, where the
    // Editor's Deny at the same node outranks it.
    let legacy = Grant::new(
        roles::administrator_grant_id(&person).as_str(),
        Subject::Person(person.clone()),
        roles::room_resource(peer.room()),
        KindSet::ALL,
        Answer::Allow,
        owner.clone(),
    );
    assert!(roles::is_legacy_administrator_grant(peer.room(), &legacy));
    peer.governance_mut()
        .put_governed_grants(std::slice::from_ref(&legacy), &host_key, now)
        .unwrap();
    assert!(is_administrator(&peer, user, now));
    assert_eq!(
        governs(&peer, editor.device_public_key, now),
        0,
        "the Editor's Deny outranks the grant where it sits"
    );

    assert!(
        peer.governance_mut()
            .reconcile_administrator_grants(&host_key, now)
            .unwrap()
    );
    assert_eq!(governs(&peer, editor.device_public_key, now), EVERY);
    assert!(is_administrator(&peer, user, now));
    let held = peer.permissions(editor.device_public_key, now).unwrap();
    let rewritten = held
        .policy
        .grants
        .iter()
        .filter(|grant| grant.id == legacy.id)
        .collect::<Vec<_>>();
    assert_eq!(
        rewritten,
        [&roles::administrator_grant(peer.room(), &person, owner)]
    );
    assert!(
        !peer
            .governance_mut()
            .reconcile_administrator_grants(&host_key, now)
            .unwrap()
    );
}

#[test]
fn reconciling_a_room_without_old_grants_writes_nothing_and_needs_no_writer() {
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let stranger = SigningKey::from_bytes(&[77; 32]);
    assert!(
        !peer
            .governance_mut()
            .reconcile_administrator_grants(&stranger, now)
            .unwrap()
    );
}

/// An administrator moderates the room as a Moderator does, an Editor not:
/// the grant comes and goes with the administrator's.
#[test]
fn an_administrator_moderates_and_an_editor_does_not() {
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let node = TreePath::room(peer.room())
        .app("so.tilo.chat")
        .kind("chat.message")
        .resource("welcome")
        .field(roles::MODERATION_FIELD)
        .action("remove");
    let moderates = |peer: &StatePeer| {
        peer.permissions(editor.device_public_key, now)
            .unwrap()
            .allows(&node, tilo_action_policy::ActionKind::Undoable, now)
    };
    assert!(!moderates(&peer));
    assert!(
        peer.governance_mut()
            .set_member_administrator(user, &host_key, now)
            .unwrap()
    );
    assert!(moderates(&peer));
    peer.governance_mut()
        .set_member_role(user, crate::join::MemberRole::Editor, &host_key, now)
        .unwrap();
    assert!(!moderates(&peer));
}
