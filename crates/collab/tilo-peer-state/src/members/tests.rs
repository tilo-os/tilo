//! What the operation log promises: every device holding the same operations
//! holds the same membership, an operation its author could not make changes
//! nothing, and the rows are the operations' projection.
use std::collections::BTreeMap;

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};
use tilo_action_policy::{
    Answer, Grant, GrantId, KindSet, Role, Subject, TreePath, WorkspacePolicy,
};
use tilo_user_identity::DeviceCert;

use super::op::tests_support::device;
use super::*;

const WORKSPACE: &str = "5f0e5f1e-6a6c-4f2a-9f9e-2b0a2d5f4c31";

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

/// What `cert` holds after `membership`: the operation replay is asked to
/// authorise.
fn project(
    cert: &DeviceCert,
    key: &ed25519_dalek::SigningKey,
    parents: Vec<OpId>,
    membership: &WorkspacePolicy,
) -> SignedMembershipOp {
    SignedMembershipOp::signed(
        cert,
        key,
        parents,
        1_700_000_000_000_000_000,
        MembershipOpCommand::Project {
            membership: Box::new(Membership::of(membership)),
        },
    )
    .unwrap()
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
    let mut held = doc.clone();
    for op in ops {
        legacy::put_op(&mut held, op).unwrap();
    }
    held
}

fn with_role(membership: &WorkspacePolicy, who: &DeviceCert, role: Role) -> WorkspacePolicy {
    let mut next = membership.clone();
    next.members.insert(person(who), role);
    next
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
    let start = holding(&doc, &[genesis.clone()]);
    let id = genesis.id().unwrap();
    let base = replay(&start, WORKSPACE);

    // The administrator admits the guest; the owner, who has not seen that,
    // removes the administrator. Both name the genesis as their parent.
    let admit = project(
        &admin,
        &admin_key,
        vec![id],
        &with_role(&base, &guest, Role::Viewer),
    );
    let mut without_admin = base.clone();
    without_admin.members.remove(&person(&admin));
    let remove = project(&owner, &owner_key, vec![id], &without_admin);

    let forwards = holding(&start, &[admit.clone(), remove.clone()]);
    let backwards = holding(&start, &[remove.clone(), admit.clone()]);
    assert_eq!(replay(&forwards, WORKSPACE), replay(&backwards, WORKSPACE));
    // The owner's change is applied first, so the administrator's admission of
    // the guest names a membership reached after it — which the administrator
    // did not hold, so it changes nothing on either device.
    let reached = replay(&forwards, WORKSPACE);
    assert_eq!(reached.members.get(&person(&admin)), None);
    assert!(!reached.members.contains_key(&person(&guest)));
    let _ = guest_key;
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
    let start = holding(&doc, &[genesis.clone()]);
    let genesis_id = genesis.id().unwrap();
    let base = replay(&start, WORKSPACE);

    let admit = project(
        &admin,
        &admin_key,
        vec![genesis_id],
        &with_role(&base, &guest, Role::Viewer),
    );
    let remove = project(&owner, &owner_key, vec![genesis_id], &{
        let mut next = base.clone();
        next.members.remove(&person(&admin));
        next
    });
    let held = holding(&start, &[admit.clone(), remove]);
    let reached = replay(&held, WORKSPACE);
    assert!(!reached.members.contains_key(&person(&guest)));

    // The removed administrator writes on top of their own admission, which is
    // in the log but was never applied: still nothing.
    let later = project(
        &admin,
        &admin_key,
        vec![admit.id().unwrap()],
        &with_role(&reached, &guest, Role::Editor),
    );
    let held = holding(&held, &[later]);
    assert_eq!(replay(&held, WORKSPACE), reached);
    let _ = guest_key;
}

/// A member who may not grant themself anything writes an operation that does:
/// it is held, and it changes nothing, on every device.
#[test]
fn a_projection_its_author_could_not_make_changes_nothing() {
    let (owner_key, owner) = device(1);
    let (viewer_key, viewer) = device(2);
    let mut doc = rows(&owner, &[(&viewer, Role::Viewer)], Vec::new());
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, &[genesis.clone()]);
    let base = replay(&start, WORKSPACE);
    assert_eq!(base.members.get(&person(&viewer)), Some(&Role::Viewer));

    // A viewer promoting themself to the owner (structural), and to a
    // moderator (out of their rank).
    let mut as_owner = base.clone();
    as_owner.members.insert(person(&viewer), Role::Owner);
    let mut as_moderator = base.clone();
    as_moderator
        .members
        .insert(person(&viewer), Role::Moderator);
    let held = holding(
        &start,
        &[
            project(&viewer, &viewer_key, vec![genesis.id().unwrap()], &as_owner),
            project(
                &viewer,
                &viewer_key,
                vec![genesis.id().unwrap()],
                &as_moderator,
            ),
        ],
    );
    assert_eq!(replay(&held, WORKSPACE), base);
}

/// A grant changes membership without touching a member row: the grant's own
/// rows are what the operation carries, and removing it takes the authority
/// back.
#[test]
fn grants_are_the_operations_they_name() {
    let (owner_key, owner) = device(1);
    let (member_key, member) = device(2);
    let mut doc = rows(&owner, &[(&member, Role::Editor)], Vec::new());
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, &[genesis.clone()]);
    let base = replay(&start, WORKSPACE);
    let grant = Grant::new(
        "app",
        Subject::Person(person(&member)),
        TreePath::workspace(WORKSPACE),
        KindSet::READ,
        Answer::Allow,
        Subject::Person(person(&owner)),
    );
    let mut granted = base.clone();
    granted.grants.push(grant.clone());
    let op = project(&member, &member_key, vec![genesis.id().unwrap()], &granted);
    let held = holding(&start, &[op]);
    // The member may not give itself a grant: only the owner and those the
    // grant's reach allows hand out authority.
    assert_eq!(replay(&held, WORKSPACE), base);
    let op = project(&owner, &owner_key, vec![genesis.id().unwrap()], &granted);
    let held = holding(&start, &[op]);
    assert_eq!(replay(&held, WORKSPACE), sorted(&granted));
    assert_eq!(replay(&held, WORKSPACE).grants.len(), base.grants.len() + 1);
}

/// The rows are the projection of the operations: settling a document writes
/// them back to what replay gives, and settling again writes nothing.
#[test]
fn settling_projects_the_operations_into_the_rows() {
    let (owner_key, owner) = device(1);
    let (_, member) = device(2);
    let mut doc = rows(&owner, &[(&member, Role::Editor)], Vec::new());
    let genesis = genesis(&owner, &owner_key, &mut doc);
    let start = holding(&doc, &[genesis.clone()]);
    let base = replay(&start, WORKSPACE);

    // A last-writer-wins row change that no operation makes.
    let mut mutated = start.clone();
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
    let start = holding(&doc, &[genesis.clone()]);
    assert_eq!(growth(Some(&doc), &start).unwrap(), 1);
    assert_eq!(growth(Some(&start), &start).unwrap(), 0);

    let mut dropped = start.clone();
    let held = map(&dropped, MEMBERS_KEY);
    dropped
        .delete(&held, genesis.id().unwrap().to_key().as_str())
        .unwrap();
    assert!(growth(Some(&start), &dropped).is_err());

    let mut changed = start.clone();
    let held = map(&changed, MEMBERS_KEY);
    changed
        .put(
            &held,
            genesis.id().unwrap().to_key(),
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
    let mut membership = WorkspacePolicy {
        workspace: WORKSPACE.to_owned(),
        owner: Some(person(&cert)),
        members: BTreeMap::from([(hex::encode([7; 32]), Role::Editor)]),
        joined: BTreeMap::from([(hex::encode([7; 32]), 1_700_000_000_000)]),
        grants: Vec::new(),
    };
    membership.grants.push(Grant::new(
        "g",
        Subject::Person(person(&cert)),
        TreePath::workspace(WORKSPACE),
        KindSet::READ,
        Answer::Allow,
        Subject::Person(person(&cert)),
    ));
    let op = project(&cert, &key, Vec::new(), &membership);
    let bytes = op.to_bytes().unwrap();
    assert_eq!(
        hex::encode(blake3::hash(&bytes).as_bytes()),
        op.id().unwrap().to_string()
    );
    let read = SignedMembershipOp::verified(&bytes).unwrap();
    assert_eq!(read, op);
    let MembershipOpCommand::Project { membership: read } = &read.command else {
        panic!("a projection reads back as a projection");
    };
    assert_eq!(read.policy(), membership);
    let _ = GrantId::new("g");
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
    doc.put(&hosts, "relay", "tilo.coffee").unwrap();
    doc.put_object(ROOT, "invitation_receipts_v1", ObjType::Map)
        .unwrap();
    doc.commit();

    let genesis = genesis(&owner, &owner_key, &mut doc);
    let mut held = holding(&doc, &[genesis.clone()]);
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
#[test]
fn a_lounge_shaped_workspace_starts_from_its_rows_unchanged() {
    use tilo_action_policy::{bot, roles};
    use tilo_workspace_join::delegation::{Delegate, DelegateScope, Delegation};

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
    let installation = "0f5a2c1e-6a4b-4c2d-9d8f-0a1b2c3d4e5f";
    let rules = [tilo_action_policy::roles::Rule::new(
        TreePath::workspace(WORKSPACE)
            .app("chat")
            .resource("message"),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
    )];
    put_grants(
        &mut doc,
        bot::grants(WORKSPACE, installation, &person(&co_owner), &rules)
            .unwrap()
            .into_iter()
            .chain([roles::block_grant(
                WORKSPACE,
                &person(&banned),
                Subject::Person(person(&relay)),
            )])
            .collect(),
    );
    let identities = map(&doc, "peer_identities_v1");
    let delegation = Delegation {
        user_public_key: co_owner.user_public_key,
        device_public_key: bot_device.device_public_key,
        delegate: Delegate::Bot {
            installation: installation.to_owned(),
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
        opaque_json: r#"{"url":"wss://tilo.coffee"}"#.to_owned(),
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
            .starts_with(&bot::grant_prefix(installation))
    }));
    assert!(legacy.is_banned(&person(&banned)));
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
