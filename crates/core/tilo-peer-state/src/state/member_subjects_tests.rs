//! Whom a change of the workspace's members is about is what its signed operation names, and
//! only an operation the workspace applied: never the change's message, which its writer says
//! whatever it likes. A bridge acting on the people a change names (a ban there for a ban here)
//! acts on that person or on no one.
use super::*;
use crate::workspace::base::{KeptChange, named_changes};
use tilo_action_policy::Role;
use tilo_action_policy::roles::governance;

/// The changes of `doc`'s governance whose message names `verb`.
fn named(doc: &AutoCommit, verb: &str) -> Vec<KeptChange> {
    named_changes(doc)
        .into_iter()
        .filter(|kept| {
            collab_types::Stamp::of_message(&kept.message).is_some_and(|stamp| stamp.verb == verb)
        })
        .collect()
}

/// Everyone the changes are about.
fn people(changes: &[KeptChange]) -> BTreeSet<String> {
    changes
        .iter()
        .flat_map(|kept| kept.subjects.iter().cloned())
        .collect()
}

#[test]
fn a_member_change_is_about_the_person_its_signed_operation_names_and_no_one_else() {
    let (mut peer, authority, _, _, now) = crate::admission_tests::seeded();
    let (_, _, ada_root) =
        crate::admission_tests::add_member_with(&mut peer, &authority, now, 7, 8);
    let (_, _, bo_root) =
        crate::admission_tests::add_member_with(&mut peer, &authority, now, 9, 10);
    let (ada, bo) = (hex::encode(ada_root), hex::encode(bo_root));
    peer.governance_mut()
        .ban_member(ada_root, &authority, now)
        .unwrap();
    let doc = peer.admitted_workspace().unwrap().clone();
    let bans = named(&doc, governance::BAN);
    assert_eq!(
        people(&bans),
        BTreeSet::from([ada.clone()]),
        "the ban is about Ada, whom its operation names: {bans:?}"
    );

    // A message naming an action the operation did not take names no one: Bo's role changed
    // under a message that says "ban" is no ban of Bo, nor of anyone.
    peer.stamp_action(
        governance::BAN,
        tilo_member_types::Actor::Member(tilo_member_types::MemberId::person(ada.clone())),
    );
    peer.governance_mut()
        .set_member_role(bo_root, Role::Viewer, &authority, now)
        .unwrap();
    peer.clear_action_stamp();
    let forged: Vec<KeptChange> = named(peer.admitted_workspace().unwrap(), governance::BAN)
        .into_iter()
        .filter(|kept| !bans.iter().any(|ban| ban.change == kept.change))
        .collect();
    assert!(!forged.is_empty(), "the mislabelled change is named");
    assert!(
        forged.iter().all(|kept| kept.subjects.is_empty()),
        "a role change labelled a ban is about no one: {forged:?}"
    );

    // A change that writes Ada's ban operation again, under the ban's own message, is not that
    // ban: the operation is the change's that wrote it first.
    let mut copied = doc.clone();
    let (key, bytes) = crate::members::replay::raw(&doc)
        .into_iter()
        .find(|(_, bytes)| {
            crate::members::SignedMembershipOp::verified(bytes).is_ok_and(|op| {
                matches!(&op.command, crate::members::MembershipOpCommand::Project { changes }
                    if changes.iter().any(|change| matches!(change,
                        tilo_action_policy::Change::Ban { person } if *person == ada)))
            })
        })
        .expect("the ban's operation is held");
    // Put over its own removal, as a value equal to the one held is no write at all.
    let key = format!("{}/{key}", crate::members::MEMBERS_KEY);
    copied.delete(ROOT, key.as_str()).unwrap();
    copied
        .put(ROOT, key.as_str(), automerge::ScalarValue::Bytes(bytes))
        .unwrap();
    copied.commit_with(
        automerge::transaction::CommitOptions::default().with_message(bans[0].message.clone()),
    );
    let copy = copied
        .get_last_local_change()
        .expect("the copy is a change")
        .hash();
    let again = named_changes(&copied);
    let rewritten = again
        .iter()
        .find(|kept| kept.change == copy)
        .expect("the copy is named");
    assert!(
        rewritten.subjects.is_empty(),
        "an operation written again is no new action on its person: {rewritten:?}"
    );
    assert_eq!(
        people(&named(&copied, governance::BAN)),
        BTreeSet::from([ada]),
        "no ban here is about Bo, nor anyone but Ada: {again:?}"
    );
    assert!(
        named(peer.admitted_workspace().unwrap(), governance::BAN)
            .iter()
            .all(|kept| !kept.subjects.contains(&bo)),
        "the mislabelled role change never surfaces as a ban of Bo"
    );
}
