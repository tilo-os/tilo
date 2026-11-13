//! What the operation log promises: a workspace starts as its owner's, every
//! device holding the same operations holds the same membership, and an
//! operation its author could not make changes nothing.
use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Change, Role, WorkspacePolicy, roles};
use tilo_user_identity::DeviceCert;

use super::op::tests_support::device;
use super::*;

const WORKSPACE: &str = "5f0e5f1e-6a6c-4f2a-9f9e-2b0a2d5f4c31";
const AT_NS: u64 = 1_700_000_000_000_000_000;

fn person(cert: &DeviceCert) -> String {
    hex::encode(cert.user_public_key)
}

/// A workspace document naming `owner` its owner, and nothing else.
fn workspace(owner: &DeviceCert) -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "room", WORKSPACE).unwrap();
    doc.put(ROOT, "owner", person(owner)).unwrap();
    doc
}

/// `changes`, written by `by` on top of `parents`, `step` nanoseconds in.
fn op(
    by: &(SigningKey, DeviceCert),
    parents: Vec<OpId>,
    step: u64,
    changes: Vec<Change>,
) -> SignedMembershipOp {
    SignedMembershipOp::signed(
        &by.1,
        &by.0,
        parents,
        AT_NS.saturating_add(step),
        MembershipOpCommand::Project { changes },
    )
    .unwrap()
}

fn id(op: &SignedMembershipOp) -> OpId {
    op.id().unwrap()
}

fn adding(who: &DeviceCert, role: Role) -> Change {
    Change::AddMember {
        person: person(who),
        role,
    }
}

/// `doc` holding `ops`, put in the order given.
fn holding(doc: &AutoCommit, ops: &[&SignedMembershipOp]) -> AutoCommit {
    let mut doc = doc.clone();
    for op in ops {
        put_op(&mut doc, op).unwrap();
    }
    doc.commit();
    doc
}

#[test]
fn a_workspace_with_no_operations_is_its_owners_alone() {
    let owner = device(1);
    let policy = replay(&workspace(&owner.1), WORKSPACE);
    let mut expected = WorkspacePolicy::new(WORKSPACE, person(&owner.1)).with_current_roles();
    expected
        .grants
        .sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
    assert_eq!(policy, expected);
    assert!(policy.members.is_empty());
    assert_eq!(
        people(&workspace(&owner.1)),
        Some([person(&owner.1)].into())
    );
}

#[test]
fn every_device_reaches_the_same_membership_from_the_same_operations() {
    let (owner, ada, bo, cy) = (device(1), device(3), device(5), device(7));
    let doc = workspace(&owner.1);
    let first = op(
        &owner,
        Vec::new(),
        1,
        vec![adding(&ada.1, Role::Editor), adding(&bo.1, Role::Viewer)],
    );
    // Written at once on top of the same membership: the owner promotes bo
    // while ada removes them.
    let promote = op(
        &owner,
        vec![id(&first)],
        2,
        vec![Change::SetRole {
            person: person(&bo.1),
            role: Role::Editor,
        }],
    );
    let remove = op(
        &ada,
        vec![id(&first)],
        3,
        vec![Change::RemoveMember {
            person: person(&bo.1),
        }],
    );
    let add = op(
        &owner,
        vec![id(&promote)],
        4,
        vec![adding(&cy.1, Role::Viewer)],
    );
    let all = [&first, &promote, &remove, &add];
    let reached = replay(&holding(&doc, &all), WORKSPACE);
    for order in [[3, 2, 1, 0], [0, 2, 1, 3], [1, 3, 0, 2]] {
        let ops: Vec<&SignedMembershipOp> = order.iter().map(|at| all[*at]).collect();
        assert_eq!(replay(&holding(&doc, &ops), WORKSPACE), reached);
    }
    assert_eq!(reached.members.get(&person(&ada.1)), Some(&Role::Editor));
    assert_eq!(reached.members.get(&person(&cy.1)), Some(&Role::Viewer));
    assert_eq!(heads(&ops(&holding(&doc, &all))), {
        let mut heads = vec![id(&remove), id(&add)];
        heads.sort();
        heads
    });
}

#[test]
fn an_operation_its_author_could_not_make_changes_nothing() {
    let (owner, viewer, stranger) = (device(1), device(3), device(5));
    let doc = workspace(&owner.1);
    let first = op(&owner, Vec::new(), 1, vec![adding(&viewer.1, Role::Viewer)]);
    // A viewer may not add anyone: the allowed half of their operation goes
    // with the refused one.
    let refused_op = op(
        &viewer,
        vec![id(&first)],
        2,
        vec![adding(&stranger.1, Role::Viewer)],
    );
    let held = replay(&holding(&doc, &[&first]), WORKSPACE);
    assert!(refused(&holding(&doc, &[&first]), &held, &refused_op).is_some());
    assert_eq!(
        replay(&holding(&doc, &[&first, &refused_op]), WORKSPACE),
        held
    );
    // And someone who is no member's operation changes nothing either.
    let outsider = op(
        &stranger,
        Vec::new(),
        3,
        vec![adding(&stranger.1, Role::Editor)],
    );
    assert_eq!(
        replay(&holding(&doc, &[&first, &refused_op, &outsider]), WORKSPACE),
        held
    );
}

#[test]
fn grants_and_roles_are_the_operations_they_name() {
    let (owner, ada) = (device(1), device(3));
    let doc = workspace(&owner.1);
    let ada_person = person(&ada.1);
    let granted = roles::administrator_grant(
        WORKSPACE,
        &ada_person,
        tilo_action_policy::Grantee::person(person(&owner.1)),
    );
    let first = op(
        &owner,
        Vec::new(),
        1,
        vec![adding(&ada.1, Role::Editor), Change::Grant(granted.clone())],
    );
    let policy = replay(&holding(&doc, &[&first]), WORKSPACE);
    assert!(policy.grants.contains(&granted));
    let joined = policy.joined.get(&ada_person).copied();
    assert_eq!(joined, Some(AT_NS / 1_000_000));
    // Removing them takes their grants and their admission time with them;
    // inviting them back admits them anew.
    let removed = op(
        &owner,
        vec![id(&first)],
        2_000_000,
        vec![Change::RemoveMember {
            person: ada_person.clone(),
        }],
    );
    let policy = replay(&holding(&doc, &[&first, &removed]), WORKSPACE);
    assert!(!policy.members.contains_key(&ada_person));
    assert!(!policy.grants.iter().any(|grant| grant.id == granted.id));
    let back = op(
        &owner,
        vec![id(&removed)],
        4_000_000,
        vec![adding(&ada.1, Role::Viewer)],
    );
    let policy = replay(&holding(&doc, &[&first, &removed, &back]), WORKSPACE);
    assert_eq!(policy.members.get(&ada_person), Some(&Role::Viewer));
    assert_eq!(
        policy.joined.get(&ada_person),
        Some(&(AT_NS / 1_000_000 + 4))
    );
}

#[test]
fn held_operations_are_kept_and_new_ones_prove_their_writer() {
    let owner = device(1);
    let doc = workspace(&owner.1);
    let first = op(
        &owner,
        Vec::new(),
        1,
        vec![adding(&device(3).1, Role::Viewer)],
    );
    let before = holding(&doc, &[&first]);
    let second = op(
        &owner,
        vec![id(&first)],
        2,
        vec![adding(&device(5).1, Role::Viewer)],
    );
    assert_eq!(growth(Some(&before), &holding(&before, &[&second])), Ok(1));
    let mut removed = before.clone();
    removed
        .delete(ROOT, op::op_key(id(&first)).as_str())
        .unwrap();
    assert!(growth(Some(&before), &removed).is_err());
    let mut forged = before.clone();
    let mut bytes = second.to_bytes().unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    forged
        .put(
            ROOT,
            op::op_key(id(&second)).as_str(),
            automerge::ScalarValue::Bytes(bytes),
        )
        .unwrap();
    assert!(growth(Some(&before), &forged).is_err());
}

/// A write on top of every head is applied to the membership remembered for
/// the state before it; the carried membership is checked against a fresh
/// replay inside [`replay::carry`] in every test.
#[test]
fn an_operation_on_every_head_is_carried_onto_the_remembered_membership() {
    let (owner, ada) = (device(1), device(3));
    let mut doc = workspace(&owner.1);
    doc.commit();
    let _ = replay(&doc, WORKSPACE);
    let before = doc.get_heads();
    let first = op(&owner, Vec::new(), 1, vec![adding(&ada.1, Role::Editor)]);
    put_op(&mut doc, &first).unwrap();
    let written = doc.get_changes(&before);
    replay::carry(&before, &doc, &written, WORKSPACE);
    let carried = replay(&doc, WORKSPACE);
    assert_eq!(carried.members.get(&person(&ada.1)), Some(&Role::Editor));
}

/// `doc` as a 0.1 release kept it: an earlier operation and the rows it
/// projected the membership into, each a JSON value by person.
fn kept_by_an_earlier_release(doc: &AutoCommit, rows: &[(&str, &str, Vec<u8>)]) -> AutoCommit {
    let mut doc = doc.clone();
    doc.put(
        ROOT,
        format!("{}/genesis", earlier::EARLIER_KEY),
        vec![0_u8],
    )
    .unwrap();
    for (row, name, value) in rows {
        let map = match doc.get(ROOT, *row).unwrap() {
            Some((_, map)) => map,
            None => doc.put_object(ROOT, *row, automerge::ObjType::Map).unwrap(),
        };
        doc.put(&map, *name, value.clone()).unwrap();
    }
    doc.commit();
    doc
}

#[test]
fn a_membership_an_earlier_release_kept_reads_the_same_once_brought_up() {
    let owner = device(1);
    let ana = device(3);
    let ben = device(5);
    let kept = kept_by_an_earlier_release(
        &workspace(&owner.1),
        &[
            (
                "members",
                &person(&ben.1),
                serde_json::to_vec(&Role::Viewer).unwrap(),
            ),
            (
                "members",
                &person(&ana.1),
                serde_json::to_vec(&Role::Editor).unwrap(),
            ),
            (
                "members",
                &person(&owner.1),
                serde_json::to_vec(&Role::Owner).unwrap(),
            ),
            (
                "members_joined_v1",
                &person(&ben.1),
                serde_json::to_vec(&2_000_u64).unwrap(),
            ),
            (
                "members_joined_v1",
                &person(&ana.1),
                serde_json::to_vec(&1_000_u64).unwrap(),
            ),
        ],
    );
    // Read as this layout reads it, the workspace is its owner's alone.
    assert!(replay(&kept, WORKSPACE).members.is_empty());
    assert!(earlier::holds_earlier_log(&kept));

    let membership = earlier::membership(&kept, WORKSPACE).unwrap();
    assert_eq!(
        earlier::roles(&membership),
        [
            (person(&ana.1), Some(Role::Editor)),
            (person(&ben.1), Some(Role::Viewer)),
        ]
        .into()
    );
    let mut brought_up = kept;
    let written = earlier::found(
        &mut brought_up,
        WORKSPACE,
        &membership,
        (&owner.1, &owner.0),
        AT_NS,
    )
    .unwrap();
    assert_eq!(written, 2, "one operation per member, no grants to add");
    let replayed = replay(&brought_up, WORKSPACE);
    assert_eq!(earlier::roles(&replayed), earlier::roles(&membership));
    assert_eq!(
        replayed.joined.get(&person(&ana.1)),
        Some(&1_000),
        "each member joined when the earlier release says they did"
    );
    assert!(!earlier::holds_earlier_log(&brought_up));
    assert_eq!(earlier::earlier_keys(&brought_up).len(), 3);
}
