//! What the operation log promises: every device holding the same operations
//! holds the same membership, an operation its author could not make changes
//! nothing, and the rows are the operations' projection.
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use tilo_action_policy::{
    Answer, Change, Grant, GrantId, KindSet, Role, Subject, TreePath, WorkspacePolicy,
};
use tilo_user_identity::DeviceCert;

use super::op::tests_support::device;
use super::*;

const WORKSPACE: &str = "5f0e5f1e-6a6c-4f2a-9f9e-2b0a2d5f4c31";
/// The bot the Lounge-shaped fixture installs.
const LOUNGE_BOT: &str = "0f5a2c1e-6a4b-4c2d-9d8f-0a1b2c3d4e5f";

fn person(cert: &DeviceCert) -> String {
    hex::encode(cert.user_public_key)
}

/// A workspace document the last-writer-wins way: the owner, who is in it, and
/// their grants.
fn rows(owner: &DeviceCert, members: &[(&DeviceCert, Role)], grants: Vec<Grant>) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", WORKSPACE).unwrap();
    doc.put(ROOT, "owner", person(owner)).unwrap();
    let members_map = doc.put_object(ROOT, "members", ObjType::Map).unwrap();
    for (member, role) in members {
        crate::state::put_member(&mut doc, &members_map, member.user_public_key, *role).unwrap();
        crate::state::put_member_joined_once(&mut doc, member.user_public_key, 1_700_000_000_000)
            .unwrap();
    }
    doc.put_object(ROOT, "grants", ObjType::Map).unwrap();
    put_grants(&mut doc, grants);
    doc.commit();
    doc
}

fn map(doc: &AutoCommit, key: &str) -> automerge::ObjId {
    doc.get(ROOT, key).unwrap().unwrap().1
}

fn put_grants(doc: &mut AutoCommit, grants: Vec<Grant>) {
    let grants_map = map(doc, "grants");
    for grant in grants {
        doc.put(
            &grants_map,
            grant.id.as_str(),
            automerge::ScalarValue::Bytes(serde_json::to_vec(&grant).unwrap()),
        )
        .unwrap();
    }
}

fn admin_grant(owner: &DeviceCert, of: &DeviceCert) -> Grant {
    tilo_action_policy::roles::administrator_grant(
        WORKSPACE,
        &person(of),
        Subject::Person(person(owner)),
    )
}

fn policy(doc: &AutoCommit) -> WorkspacePolicy {
    sorted(&tilo_desktop_model::projection::workspace_policy_of(
        doc, WORKSPACE,
    ))
}

/// A membership with its grants in the order every device reads them in: a
/// document keeps its grant rows in the order they were written, which is not
/// the same on every device, and is no part of what a workspace allows.
fn sorted(membership: &WorkspacePolicy) -> WorkspacePolicy {
    let mut membership = membership.clone();
    membership
        .grants
        .sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    membership
}

/// What `cert`'s device wrote: `changes`, in order, on top of `parents`.
fn project(
    cert: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
    parents: Vec<OpId>,
    changes: Vec<Change>,
) -> SignedMembershipOp {
    SignedMembershipOp::signed(
        cert,
        key,
        parents,
        1_700_000_000_000_000_000,
        MembershipOpCommand::Project { changes },
    )
    .unwrap()
}

/// The one change of adding `who` as a `role`.
fn adding(who: &DeviceCert, role: Role) -> Vec<Change> {
    vec![Change::AddMember {
        person: person(who),
        role,
    }]
}

/// The one change of taking `who` out.
fn removing(who: &DeviceCert) -> Vec<Change> {
    vec![Change::RemoveMember {
        person: person(who),
    }]
}

fn genesis(
    cert: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
    doc: &mut AutoCommit,
) -> SignedMembershipOp {
    legacy::genesis_op(
        cert,
        key,
        &doc.get_heads(),
        &policy(doc),
        1_700_000_000_000_000_000,
    )
    .unwrap()
}

/// A log-holding document: `doc` with `ops` written into it in the order given.
fn holding(doc: &AutoCommit, ops: &[SignedMembershipOp]) -> AutoCommit {
    // A device writes under its own actor, so two devices that each hold part
    // of the log can exchange their changes.
    let mut seed = doc.clone();
    let mut held = seed.fork();
    for op in ops {
        legacy::put_op(&mut held, op).unwrap();
    }
    held
}

#[test]
fn the_genesis_projection_is_the_membership_the_rows_held() {
    let (owner_key, owner) = device(1);
    let (_, member) = device(2);
    let mut doc = rows(
        &owner,
        &[(&member, Role::Editor)],
        vec![admin_grant(&owner, &member)],
    );
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let started = holding(&doc, &[genesis]);
    assert_eq!(replay(&started, WORKSPACE), policy(&doc));
    assert_eq!(replay(&started, WORKSPACE), policy(&started));
}

#[test]
fn a_workspace_without_a_genesis_reads_its_rows() {
    let (_, owner) = device(1);
    let (_, member) = device(2);
    let doc = rows(&owner, &[(&member, Role::Viewer)], Vec::new());
    assert!(super::replay::genesis(&ops(&doc)).is_none());
    assert!(legacy::needs_genesis(&doc));
    assert_eq!(replay(&doc, WORKSPACE), policy(&doc));
}

/// A genesis by a device that is not the owner's starts nothing: an
/// administrator cannot take the workspace's membership back to what it was
/// when they were.
#[test]
fn only_the_owners_device_starts_the_log() {
    let (owner_key, owner) = device(1);
    let (admin_key, admin) = device(2);
    let mut doc = rows(
        &owner,
        &[(&admin, Role::Moderator)],
        vec![admin_grant(&owner, &admin)],
    );
    let mut claimed = policy(&doc);
    claimed.members.insert(person(&admin), Role::Owner);
    let op = legacy::genesis_op(
        &admin,
        &admin_key,
        &doc.get_heads(),
        &claimed,
        1_700_000_000_000_000_000,
    )
    .unwrap();
    let held = holding(&doc, &[op]);
    assert_eq!(replay(&held, WORKSPACE), policy(&doc));
    // The owner's own genesis governs.
    let owner_genesis = genesis(&owner, &owner_key, &mut doc);
    let held = holding(&held, &[owner_genesis]);
    assert_eq!(replay(&held, WORKSPACE), policy(&doc));
}

/// The order operations arrive in is not the order they are applied in: every
/// device folds them by their parents, then the owner's first, an
/// administrator's next, then everyone else's, then by id.
#[test]
fn every_device_reaches_the_same_membership_from_the_same_operations() {
    let (owner_key, owner) = device(1);
    let (admin_key, admin) = device(2);
    let (guest_key, guest) = device(3);
    let mut doc = rows(
        &owner,
        &[(&admin, Role::Moderator)],
        vec![admin_grant(&owner, &admin)],
    );
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, std::slice::from_ref(&genesis));
    let id = genesis.id().unwrap();

    // The administrator admits the guest; the owner, who has not seen that,
    // removes the administrator. Both name the genesis as their parent.
    let admits_guest = project(&admin, &admin_key, vec![id], adding(&guest, Role::Viewer));
    let remove = project(&owner, &owner_key, vec![id], removing(&admin));

    let forwards = holding(&start, &[admits_guest.clone(), remove.clone()]);
    let backwards = holding(&start, &[remove, admits_guest]);
    assert_eq!(replay(&forwards, WORKSPACE), replay(&backwards, WORKSPACE));
    // The owner's change is applied first, so the administrator's admission of
    // the guest names a membership reached after it — which the administrator
    // did not hold, so it changes nothing on either device.
    let reached = replay(&forwards, WORKSPACE);
    assert_eq!(reached.members.get(&person(&admin)), None);
    assert!(!reached.members.contains_key(&person(&guest)));
    let _ = guest_key;
}

/// A workspace document's membership rows, as stored: the bytes every device
/// that projects the same membership writes.
fn projected_rows(doc: &AutoCommit) -> Vec<(String, Vec<u8>)> {
    let mut rows = Vec::new();
    for key in ["members", "members_joined_v1", "grants"] {
        let held = map(doc, key);
        for name in doc.keys(&held) {
            let Some((automerge::Value::Scalar(value), _)) = doc.get(&held, name.as_str()).unwrap()
            else {
                panic!("a row is a scalar");
            };
            let automerge::ScalarValue::Bytes(bytes) = value.as_ref() else {
                panic!("a row is bytes");
            };
            rows.push((format!("{key}/{name}"), bytes.clone()));
        }
    }
    rows
}

/// `doc` settled, and the rows it projects.
fn settled(doc: &AutoCommit) -> (AutoCommit, Vec<(String, Vec<u8>)>) {
    let mut doc = doc.clone();
    settle(&mut doc, WORKSPACE).unwrap();
    let rows = projected_rows(&doc);
    (doc, rows)
}

/// Every device that holds the same operations holds the same membership, in
/// whatever order the operations arrived: two owners removing each other at
/// the same time resolve one way on every device, and the operation that does
/// not take is kept with no effect.
#[test]
fn two_owners_removing_each_other_reach_the_same_membership_in_any_order() {
    let (owner_key, owner) = device(1);
    let (co_owner_key, co_owner) = device(2);
    let mut doc = rows(&owner, &[(&co_owner, Role::Owner)], Vec::new());
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, std::slice::from_ref(&genesis));
    let id = genesis.id().unwrap();

    // The owner removes the co-owner; the co-owner removes the owner.
    let remove_co_owner = project(&owner, &owner_key, vec![id], removing(&co_owner));
    let remove_owner = project(&co_owner, &co_owner_key, vec![id], removing(&owner));

    let forwards = holding(&start, &[remove_co_owner.clone(), remove_owner.clone()]);
    let backwards = holding(&start, &[remove_owner, remove_co_owner]);
    assert_eq!(replay(&forwards, WORKSPACE), replay(&backwards, WORKSPACE));
    let reached = replay(&forwards, WORKSPACE);
    assert!(reached.is_owner(&person(&owner)), "the owner stays");
    assert_eq!(reached.role_of(&person(&co_owner)), None);
    assert_eq!(ops(&forwards).len(), 3, "the refused operation is kept");

    let (_, forwards_rows) = settled(&forwards);
    let (_, backwards_rows) = settled(&backwards);
    assert_eq!(
        forwards_rows, backwards_rows,
        "the rows every device writes"
    );
}

/// A role change racing the removal of the member who made it: the owner's
/// operation is applied first wherever the two arrive, so the role change names
/// a membership it was not made in and changes nothing, on every device.
#[test]
fn a_role_change_racing_a_removal_reaches_the_same_membership_in_any_order() {
    let (owner_key, owner) = device(1);
    let (admin_key, admin) = device(2);
    let (_, viewer) = device(3);
    let mut doc = rows(
        &owner,
        &[(&admin, Role::Moderator), (&viewer, Role::Viewer)],
        vec![admin_grant(&owner, &admin)],
    );
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, std::slice::from_ref(&genesis));
    let id = genesis.id().unwrap();

    let promote = project(
        &admin,
        &admin_key,
        vec![id],
        vec![Change::SetRole {
            person: person(&viewer),
            role: Role::Commenter,
        }],
    );
    let remove = project(&owner, &owner_key, vec![id], removing(&admin));

    let forwards = holding(&start, &[promote.clone(), remove.clone()]);
    let backwards = holding(&start, &[remove, promote]);
    assert_eq!(replay(&forwards, WORKSPACE), replay(&backwards, WORKSPACE));
    let reached = replay(&forwards, WORKSPACE);
    assert_eq!(reached.role_of(&person(&admin)), None);
    assert_eq!(
        reached.role_of(&person(&viewer)),
        Some(Role::Viewer),
        "the removed administrator's role change changed nothing"
    );
    let (_, forwards_rows) = settled(&forwards);
    let (_, backwards_rows) = settled(&backwards);
    assert_eq!(forwards_rows, backwards_rows);
}

/// Two administrators inviting at the same time both admit, and neither
/// operation takes the other's away; when both invite the same person, both
/// devices admit them once, by the operation the order reaches first.
#[test]
fn two_admins_inviting_at_the_same_time_both_admit() {
    let (owner_key, owner) = device(1);
    let (first_key, first) = device(2);
    let (second_key, second) = device(3);
    let (_, guest_a) = device(4);
    let (_, guest_b) = device(5);
    let mut doc = rows(
        &owner,
        &[(&first, Role::Editor), (&second, Role::Editor)],
        vec![admin_grant(&owner, &first), admin_grant(&owner, &second)],
    );
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, std::slice::from_ref(&genesis));
    let id = genesis.id().unwrap();

    let invites_a = project(
        &first,
        &first_key,
        vec![id],
        adding(&guest_a, Role::Commenter),
    );
    let invites_b = project(
        &second,
        &second_key,
        vec![id],
        adding(&guest_b, Role::Commenter),
    );
    let forwards = holding(&start, &[invites_a.clone(), invites_b.clone()]);
    let backwards = holding(&start, &[invites_b, invites_a]);
    for held in [&forwards, &backwards] {
        let reached = replay(held, WORKSPACE);
        assert_eq!(reached.role_of(&person(&guest_a)), Some(Role::Commenter));
        assert_eq!(reached.role_of(&person(&guest_b)), Some(Role::Commenter));
    }

    // The same guest, invited by both: admitted once. Which invitation is
    // reached first is the same on every device, so the role they hold is too.
    let as_editor = project(&first, &first_key, vec![id], adding(&guest_a, Role::Editor));
    let as_commenter = project(
        &second,
        &second_key,
        vec![id],
        adding(&guest_a, Role::Commenter),
    );
    let forwards = holding(&start, &[as_editor.clone(), as_commenter.clone()]);
    let backwards = holding(&start, &[as_commenter, as_editor]);
    assert_eq!(replay(&forwards, WORKSPACE), replay(&backwards, WORKSPACE));
    let joined = replay(&forwards, WORKSPACE);
    assert!(
        joined.role_of(&person(&guest_a)).is_some(),
        "the guest is admitted once"
    );
    let (_, forwards_rows) = settled(&forwards);
    let (_, backwards_rows) = settled(&backwards);
    assert_eq!(forwards_rows, backwards_rows);
}

/// A member removed while they were writing an operation keeps that operation
/// held and loses its effect: their grants were gone before anything they wrote
/// was reached.
#[test]
fn an_operation_written_after_its_author_lost_the_grant_has_no_effect() {
    let (owner_key, owner) = device(1);
    let (admin_key, admin) = device(2);
    let (guest_key, guest) = device(3);
    let mut doc = rows(
        &owner,
        &[(&admin, Role::Moderator)],
        vec![admin_grant(&owner, &admin)],
    );
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, std::slice::from_ref(&genesis));
    let genesis_id = genesis.id().unwrap();
    let base = replay(&start, WORKSPACE);
    let _ = base;

    let admits_guest = project(
        &admin,
        &admin_key,
        vec![genesis_id],
        adding(&guest, Role::Viewer),
    );
    let remove = project(&owner, &owner_key, vec![genesis_id], removing(&admin));
    let held = holding(&start, &[admits_guest.clone(), remove]);
    let reached = replay(&held, WORKSPACE);
    assert!(!reached.members.contains_key(&person(&guest)));

    // The removed administrator writes on top of their own admission, which is
    // in the log but was never applied: still nothing.
    let later = project(
        &admin,
        &admin_key,
        vec![admits_guest.id().unwrap()],
        adding(&(device(4).1), Role::Editor),
    );
    let held = holding(&held, &[later]);
    assert_eq!(replay(&held, WORKSPACE), reached);
    let _ = guest_key;
}

/// A member who may not grant themself anything writes operations that do:
/// they are held, and they change nothing, on every device.
#[test]
fn an_operation_its_author_could_not_make_changes_nothing() {
    let (owner_key, owner) = device(1);
    let (viewer_key, viewer) = device(2);
    let mut doc = rows(&owner, &[(&viewer, Role::Viewer)], Vec::new());
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, std::slice::from_ref(&genesis));
    let base = replay(&start, WORKSPACE);
    assert_eq!(base.members.get(&person(&viewer)), Some(&Role::Viewer));

    // A viewer promoting themself to the owner (structural), and to a
    // moderator (out of their rank).
    let as_owner = vec![Change::SetRole {
        person: person(&viewer),
        role: Role::Owner,
    }];
    let as_moderator = vec![Change::SetRole {
        person: person(&viewer),
        role: Role::Moderator,
    }];
    let held = holding(
        &start,
        &[
            project(&viewer, &viewer_key, vec![genesis.id().unwrap()], as_owner),
            project(
                &viewer,
                &viewer_key,
                vec![genesis.id().unwrap()],
                as_moderator,
            ),
        ],
    );
    assert_eq!(replay(&held, WORKSPACE), base);
}

/// An operation is all of its changes or none: an author cannot put an allowed
/// change in front of one its grants do not reach, and have the first stand.
#[test]
fn one_change_its_author_could_not_make_takes_the_whole_operation() {
    let (owner_key, owner) = device(1);
    let (editor_key, editor) = device(2);
    let (_, guest) = device(3);
    let mut doc = rows(&owner, &[(&editor, Role::Editor)], Vec::new());
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, std::slice::from_ref(&genesis));
    let base = replay(&start, WORKSPACE);
    let held = holding(
        &start,
        &[project(
            &editor,
            &editor_key,
            vec![genesis.id().unwrap()],
            // An Editor may not admit anyone...
            adding(&guest, Role::Viewer),
        )],
    );
    assert_eq!(
        replay(&held, WORKSPACE),
        base,
        "the operation changes nothing"
    );
    // ...nor share the workspace it is in, so nothing it writes after that
    // stands either.
    let own_grant = Grant::new(
        "app",
        Subject::Person(person(&editor)),
        TreePath::workspace(WORKSPACE),
        KindSet::READ,
        Answer::Allow,
        Subject::Person(person(&owner)),
    );
    let mut steps = adding(&guest, Role::Viewer);
    steps.push(Change::Grant(own_grant.clone()));
    let held = holding(
        &start,
        &[project(
            &editor,
            &editor_key,
            vec![genesis.id().unwrap()],
            steps,
        )],
    );
    assert_eq!(replay(&held, WORKSPACE), base);
    assert!(
        !replay(&held, WORKSPACE)
            .grants
            .iter()
            .any(|g| g.id == own_grant.id)
    );
}

/// A grant changes membership without touching a member row: the grant's own
/// steps are what the operation carries, and removing it takes the authority
/// back.
#[test]
fn grants_are_the_operations_they_name() {
    let (owner_key, owner) = device(1);
    let (member_key, member) = device(2);
    let mut doc = rows(&owner, &[(&member, Role::Editor)], Vec::new());
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, std::slice::from_ref(&genesis));
    let base = replay(&start, WORKSPACE);
    let grant = Grant::new(
        "app",
        Subject::Person(person(&member)),
        TreePath::workspace(WORKSPACE),
        KindSet::READ,
        Answer::Allow,
        Subject::Person(person(&owner)),
    );
    let op = project(
        &member,
        &member_key,
        vec![genesis.id().unwrap()],
        vec![Change::Grant(grant.clone())],
    );
    let held = holding(&start, std::slice::from_ref(&op));
    // The member may not give itself a grant: only the owner and those the
    // grant's reach allows hand out authority.
    assert_eq!(replay(&held, WORKSPACE), base);
    let op = project(
        &owner,
        &owner_key,
        vec![genesis.id().unwrap()],
        vec![Change::Grant(grant.clone())],
    );
    let held = holding(&start, std::slice::from_ref(&op));
    let granted = replay(&held, WORKSPACE);
    assert!(granted.grants.iter().any(|held| held.id == grant.id));
    assert_eq!(granted.grants.len(), base.grants.len() + 1);
    assert_eq!(granted.members, base.members, "a grant is no member row");

    // And taking it back leaves the membership it named as it was.
    let revoke = project(
        &owner,
        &owner_key,
        vec![op.id().unwrap()],
        vec![Change::Revoke(grant.id)],
    );
    let held = holding(&held, &[revoke]);
    assert_eq!(replay(&held, WORKSPACE), base);
}

/// The rows are the projection of the operations: settling a document writes
/// them back to what replay gives, and settling again writes nothing.
#[test]
fn settling_projects_the_operations_into_the_rows() {
    let (owner_key, owner) = device(1);
    let (_, member) = device(2);
    let mut doc = rows(&owner, &[(&member, Role::Editor)], Vec::new());
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, std::slice::from_ref(&genesis));
    let base = replay(&start, WORKSPACE);

    // A last-writer-wins row change that no operation makes.
    let mut mutated = start;
    let members = map(&mutated, "members");
    crate::state::put_member(
        &mut mutated,
        &members,
        device(4).1.user_public_key,
        Role::Owner,
    )
    .unwrap();
    let mut settled = mutated.clone();
    settle(&mut settled, WORKSPACE).unwrap();
    assert_eq!(policy(&settled), base);
    let mut again = settled.clone();
    settle(&mut again, WORKSPACE).unwrap();
    assert_eq!(
        again.get_heads(),
        settled.get_heads(),
        "settling writes nothing new"
    );
}

/// A batch may add operations, never change or drop one: an operation is what
/// its author signed.
#[test]
fn held_operations_are_kept_and_new_ones_prove_their_writer() {
    let (owner_key, owner) = device(1);
    let mut doc = rows(&owner, &[], Vec::new());
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, std::slice::from_ref(&genesis));
    assert_eq!(growth(Some(&doc), &start).unwrap(), 1);
    assert_eq!(growth(Some(&start), &start).unwrap(), 0);

    let key = crate::members::op::op_key(genesis.id().unwrap());
    let mut dropped = start.clone();
    dropped.delete(ROOT, key.as_str()).unwrap();
    assert!(growth(Some(&start), &dropped).is_err());

    let mut changed = start.clone();
    changed
        .put(
            ROOT,
            key.as_str(),
            automerge::ScalarValue::Bytes(b"not an operation".to_vec()),
        )
        .unwrap();
    assert!(growth(Some(&start), &changed).is_err());
}

/// The stored bytes are the format: the operation a device writes today is the
/// operation every device reads back, byte for byte and id for id. Changing the
/// command's shape without a version is changing this test.
#[test]
fn an_operation_is_its_bytes() {
    let (key, cert) = device(1);
    let changes = vec![
        Change::AddMember {
            person: person(&cert),
            role: Role::Owner,
        },
        Change::Grant(Grant::new(
            "g",
            Subject::Person(person(&cert)),
            TreePath::workspace(WORKSPACE),
            KindSet::READ,
            Answer::Allow,
            Subject::Person(person(&cert)),
        )),
        Change::SetRole {
            person: person(&cert),
            role: Role::Editor,
        },
        Change::RemoveMember {
            person: person(&cert),
        },
        Change::Revoke(GrantId::new("g")),
    ];
    let op = project(&cert, &key, Vec::new(), changes.clone());
    let bytes = op.to_bytes().unwrap();
    assert_eq!(
        hex::encode(blake3::hash(&bytes).as_bytes()),
        op.id().unwrap().to_string()
    );
    let read = SignedMembershipOp::verified(&bytes).unwrap();
    assert_eq!(read, op);
    let MembershipOpCommand::Project { changes: read } = &read.command else {
        panic!("a projection reads back as a projection");
    };
    assert_eq!(read, &changes, "a change is its bytes, in its order");
}

/// A workspace is more than its members: bots, their installations, the people
/// who have been here and how they announce themselves. Settling projects the
/// membership and nothing else.
#[test]
fn settling_leaves_everything_that_is_not_membership_alone() {
    let (owner_key, owner) = device(1);
    let (_, member) = device(2);
    let mut doc = rows(&owner, &[(&member, Role::Editor)], Vec::new());
    for (key, value) in [
        ("name", "Lounge"),
        ("room", WORKSPACE),
        ("desktop_counter_v1", "7"),
    ] {
        doc.put(ROOT, key, value).unwrap();
    }
    let identities = doc
        .put_object(ROOT, "peer_identities_v1", ObjType::Map)
        .unwrap();
    doc.put(
        &identities,
        hex::encode(member.device_public_key),
        automerge::ScalarValue::Bytes(b"a bot's row".to_vec()),
    )
    .unwrap();
    let hosts = doc.put_object(ROOT, "hosts_v1", ObjType::Map).unwrap();
    doc.put(&hosts, "relay", "tilo.sh").unwrap();
    doc.put_object(ROOT, "invitation_receipts_v1", ObjType::Map)
        .unwrap();
    doc.commit();

    let genesis = genesis(&owner, &owner_key, &mut doc);
    let mut held = holding(&doc, std::slice::from_ref(&genesis));
    settle(&mut held, WORKSPACE).unwrap();
    assert_eq!(
        held.get(ROOT, "name").unwrap(),
        doc.get(ROOT, "name").unwrap()
    );
    assert_eq!(
        held.get(ROOT, "peer_identities_v1").unwrap(),
        doc.get(ROOT, "peer_identities_v1").unwrap()
    );
    assert_eq!(
        held.get(ROOT, "hosts_v1").unwrap(),
        doc.get(ROOT, "hosts_v1").unwrap()
    );
    assert_eq!(
        held.get(ROOT, "invitation_receipts_v1").unwrap(),
        doc.get(ROOT, "invitation_receipts_v1").unwrap()
    );
    assert_eq!(replay(&held, WORKSPACE), policy(&doc));
}

/// A Lounge-shaped workspace: the relay's owner root and its device, the
/// people the room was opened to with their roles and joining times, a bot the
/// relay installed with the grants its program gives it and the delegation it
/// acts under, the device that serves the room, and someone the room banned.
///
/// The document is built from the seed a new workspace starts from, so it is
/// the shape the relay's own document has, and the only thing the log adds is
/// the genesis: the membership comes out exactly as the rows held it, the bot
/// with its delegation and the serving device untouched.
/// The bot the Lounge installed, its delegation and the device that serves the
/// room: rows the membership projection leaves alone.
fn lounge_bot_and_host(
    doc: &mut AutoCommit,
    co_owner: &DeviceCert,
    bot_device: &DeviceCert,
    relay: &DeviceCert,
) {
    use tilo_workspace_join::delegation::{Delegate, DelegateScope, Delegation};

    let identities = map(doc, "peer_identities_v1");
    let delegation = Delegation {
        user_public_key: co_owner.user_public_key,
        device_public_key: bot_device.device_public_key,
        delegate: Delegate::Bot {
            installation: LOUNGE_BOT.to_owned(),
        },
        label: "Bridge".to_owned(),
        color: [1, 2, 3],
        scope: DelegateScope {
            rooms: vec![WORKSPACE.to_owned()],
            apps: Vec::new(),
        },
        issued_at_ns: 1_700_000_000_000_000_000,
        expires_at_ns: 1_700_000_000_000_000_000 + 30 * 24 * 3600 * 1_000_000_000,
        signature: [0; 64],
    };
    let row = crate::state::GovernedIdentity {
        device_cert: bot_device.clone(),
        revocations: Vec::new(),
        admission: None,
        admissions: Vec::new(),
        delegate: Some(crate::state::GovernedDelegateRow {
            delegation,
            withdrawn_at_ns: None,
            installation: None,
        }),
        workspace_delegations: Vec::new(),
    };
    doc.put(
        &identities,
        hex::encode(bot_device.device_public_key),
        automerge::ScalarValue::Bytes(serde_json::to_vec(&row).unwrap()),
    )
    .unwrap();
    let hosts = doc.put_object(ROOT, "hosts_v1", ObjType::Map).unwrap();
    let hint = tilo_workspace_join::TransportHint {
        kind: "relay".to_owned(),
        opaque_json: r#"{"url":"wss://tilo.sh"}"#.to_owned(),
        host: relay.clone(),
        service_public_key: [9; 32],
        host_signature: [0; 64],
    };
    doc.put(
        &hosts,
        hex::encode(relay.device_public_key),
        serde_json::to_string(&hint).unwrap().as_str(),
    )
    .unwrap();
}

fn lounge() -> (ed25519_dalek::SigningKey, DeviceCert, AutoCommit) {
    use tilo_action_policy::{bot, roles};

    let (owner_key, relay) = device(1);
    let (_, co_owner) = device(2);
    let (_, moderator) = device(3);
    let (_, member) = device(4);
    let (_, banned) = device(5);
    let (_, bot_device) = device(6);
    let mut doc = AutoCommit::new();
    for change in crate::state::workspace_seed(
        WORKSPACE,
        &relay,
        &[
            relay.clone(),
            co_owner.clone(),
            moderator.clone(),
            member.clone(),
        ],
        "Lounge",
    )
    .unwrap()
    .workspace_changes
    {
        let change = automerge::Change::from_bytes(change).unwrap();
        tilo_automerge::ApplyIncoming::apply_incoming(&mut doc, [change]).unwrap();
    }
    let members = map(&doc, "members");
    for (who, role) in [
        (&co_owner, Role::Owner),
        (&moderator, Role::Moderator),
        (&member, Role::Editor),
    ] {
        crate::state::put_member(&mut doc, &members, who.user_public_key, role).unwrap();
        crate::state::put_member_joined_once(&mut doc, who.user_public_key, 1_700_000_000_000)
            .unwrap();
    }
    let rules = [tilo_action_policy::roles::Rule::new(
        TreePath::workspace(WORKSPACE)
            .app("chat")
            .resource("message"),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
    )];
    put_grants(
        &mut doc,
        bot::grants(WORKSPACE, LOUNGE_BOT, &person(&co_owner), &rules)
            .unwrap()
            .into_iter()
            .chain([roles::block_grant(
                WORKSPACE,
                &person(&banned),
                Subject::Person(person(&relay)),
            )])
            .collect(),
    );
    lounge_bot_and_host(&mut doc, &co_owner, &bot_device, &relay);
    doc.commit();

    let legacy = policy(&doc);
    assert_eq!(
        legacy.members.get(&person(&moderator)),
        Some(&Role::Moderator)
    );
    assert!(legacy.grants.iter().any(|grant| {
        grant
            .id
            .as_str()
            .starts_with(&bot::grant_prefix(LOUNGE_BOT))
    }));
    assert!(legacy.is_banned(&person(&banned)));
    (owner_key, relay, doc)
}

#[test]
fn a_lounge_shaped_workspace_starts_from_its_rows_unchanged() {
    let (owner_key, relay, mut doc) = lounge();
    let legacy = policy(&doc);
    let genesis = genesis(&relay, &owner_key, &mut doc);
    assert_eq!(genesis.person(), person(&relay));
    let mut started = holding(&doc, &[genesis]);
    assert_eq!(replay(&started, WORKSPACE), legacy);
    // The rows already say what the log says: starting the log rewrites none
    // of them, and leaving the bot with its delegation and the serving device
    // alone is part of that.
    let written = started.get_heads();
    settle(&mut started, WORKSPACE).unwrap();
    assert_eq!(started.get_heads(), written, "genesis rewrites no row");
    assert_eq!(policy(&started), legacy);
    assert_eq!(
        started.get(ROOT, "peer_identities_v1").unwrap(),
        doc.get(ROOT, "peer_identities_v1").unwrap()
    );
    assert_eq!(
        started.get(ROOT, "hosts_v1").unwrap(),
        doc.get(ROOT, "hosts_v1").unwrap()
    );
}

/// The property the replay is for: the operations a device holds decide the
/// membership, and nothing else — not the order they arrived in, not which of
/// them came from the same device, not how two devices' operations were split
/// between them before they synced. Every replica holding the same operations
/// writes the same rows, byte for byte.
mod arrival {
    use super::*;
    use proptest::prelude::*;

    /// The devices a generated log's operations are written by: the owner, an
    /// administrator, an editor and a viewer.
    const AUTHORS: u8 = 4;
    /// The changes a generated operation's author makes.
    const CHANGES: u8 = 6;

    fn change(pick: u8) -> Vec<Change> {
        let guest = device(6).1;
        let other = device(7).1;
        let editor = device(2).1;
        match pick {
            0 => adding(&guest, Role::Commenter),
            1 => adding(&other, Role::Viewer),
            2 => removing(&guest),
            3 => vec![Change::SetRole {
                person: person(&guest),
                role: Role::Editor,
            }],
            4 => vec![Change::SetRole {
                person: person(&editor),
                role: Role::Commenter,
            }],
            _ => removing(&other),
        }
    }

    /// A workspace with the four authors in it, and the operations built for
    /// the plan: each by its author, on the genesis or on the operation before
    /// it, so any order they arrive in is one a device can reach.
    fn build(plan: &[(u8, u8, u8)]) -> (AutoCommit, Vec<SignedMembershipOp>) {
        let (owner_key, owner) = device(1);
        let (admin_key, admin) = device(2);
        let (editor_key, editor) = device(3);
        let (viewer_key, viewer) = device(4);
        let keys = [&owner_key, &admin_key, &editor_key, &viewer_key];
        let mut doc = rows(
            &owner,
            &[
                (&admin, Role::Moderator),
                (&editor, Role::Editor),
                (&viewer, Role::Viewer),
            ],
            vec![admin_grant(&owner, &admin)],
        );
        let genesis = genesis(&owner, &owner_key, &mut doc);
        let start = holding(&doc, std::slice::from_ref(&genesis));
        let mut ops = Vec::new();
        for (index, (author, on_parent, step)) in plan.iter().enumerate() {
            let (_, cert) = device(author.saturating_add(1));
            let parent = if index > 0 && *on_parent == 0 {
                ops.last().map(|op: &SignedMembershipOp| op.id().unwrap())
            } else {
                None
            };
            ops.push(project(
                &cert,
                keys[usize::from(*author)],
                parent.into_iter().collect(),
                change(*step),
            ));
        }
        (start, ops)
    }

    /// `ops` put into `start` in `order`, settled, and the rows they project.
    fn arrived(
        start: &AutoCommit,
        ops: &[SignedMembershipOp],
        order: &[usize],
    ) -> Vec<(String, Vec<u8>)> {
        let ordered: Vec<SignedMembershipOp> = order.iter().map(|at| ops[*at].clone()).collect();
        let (doc, rows) = settled(&holding(start, &ordered));
        assert_eq!(
            replay(&doc, WORKSPACE),
            replay(&holding(start, ops), WORKSPACE),
            "arrival order {order:?}"
        );
        rows
    }

    fn shuffled(length: usize, bytes: &[u8]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..length).collect();
        for (turn, byte) in bytes.iter().enumerate() {
            let at = usize::from(*byte).checked_rem(length).unwrap_or_default();
            let to = length.saturating_sub(1);
            order.swap(at, turn.checked_rem(to.max(1)).unwrap_or_default());
        }
        order
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        #[test]
        fn the_order_operations_arrive_in_never_changes_what_every_device_holds(
            plan in proptest::collection::vec((0..AUTHORS, 0..3u8, 0..CHANGES), 1..=5),
            orders in proptest::collection::vec(any::<u8>(), 64),
        ) {
            let (mut start, ops) = build(&plan);
            let expected = arrived(&start, &ops, &(0..ops.len()).collect::<Vec<_>>());
            for turn in 0..3usize {
                let seed = &orders[turn.saturating_mul(16)..turn.saturating_add(1).saturating_mul(16)];
                let order = shuffled(ops.len(), seed);
                prop_assert_eq!(arrived(&start, &ops, &order), expected.clone());
            }

            // Two devices that received half each and then synced hold what one
            // device that received all of them holds.
            let half = ops.len() / 2;
            let first = holding(&start, &ops[..half]);
            let mut second = holding(&start, &ops[half..]);
            let mut synced = first;
            let changes = second.get_changes(&start.get_heads());
            tilo_automerge::ApplyIncoming::apply_incoming(&mut synced, changes).unwrap();
            let (synced, rows) = settled(&synced);
            prop_assert_eq!(replay(&synced, WORKSPACE), replay(&holding(&start, &ops), WORKSPACE));
            prop_assert_eq!(rows, expected);
        }
    }
}

/// Rows the log never wrote (an owner named outside it) leave every change
/// its owner's device makes after them refused, and the refusal says which
/// step the log refuses and where the rows differ from it.
#[test]
fn a_change_on_rows_the_log_never_wrote_says_why_it_is_refused() {
    let (owner_key, relay, mut doc) = lounge();
    let rows = policy(&doc);
    let (_, co_owner) = device(2);
    let mut logged = rows.clone();
    logged.members.remove(&person(&co_owner));
    logged.joined.remove(&person(&co_owner));
    let genesis = legacy::genesis_op(
        &relay,
        &owner_key,
        &doc.get_heads(),
        &logged,
        1_700_000_000_000_000_000,
    )
    .unwrap();
    let started = holding(&doc, std::slice::from_ref(&genesis));
    let held = replay(&started, WORKSPACE);
    assert_ne!(held, rows);
    let differ = differences(&held, &rows);
    assert!(
        differ
            .iter()
            .any(|said| said.contains(&person(&co_owner)) && said.contains("only in the second")),
        "{differ:?}"
    );
    let op = project(
        &relay,
        &owner_key,
        vec![genesis.id().unwrap()],
        steps(&held, &rows).unwrap(),
    );
    let why = refused(&held, &op).expect("the log refuses an owner it never named");
    assert!(why.contains("OwnerRole"), "{why}");
    assert_ne!(replay(&holding(&started, &[op]), WORKSPACE), rows);
}
