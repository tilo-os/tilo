//! Who a projected standing decides for: the person whose standing the
//! governor projected, or an agent or bot they delegated the change to.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;

use tilo_action_policy::{
    ActionKind, Answer, Context, Role, RoomPolicy, Standing, Subject, TreePath, roles,
};
use tilo_actions::{ActionInvoker, ActorKind};

use super::{Claimed, claimed_subject};
use crate::workspace::Standings;

const ROOM: &str = "room-1";

/// Ada edits the room; the agent `build` was delegated what an Editor may do.
fn standings() -> Standings {
    let policy = RoomPolicy {
        room: ROOM.to_owned(),
        owner: Some("ada".to_owned()),
        members: BTreeMap::from([("ada".to_owned(), Role::Editor)]),
        joined: BTreeMap::new(),
        grants: roles::room_role_grants(ROOM, &Subject::person("ada")),
    }
    .with_current_roles();
    let standing =
        Standing::person(policy, "ada").with_bound(roles::agent_delegation(ROOM, "build", "ada"));
    Standings::from([([7; 32], standing)])
}

fn allows(standings: &Standings, invoker: Option<&ActionInvoker>) -> bool {
    let Some(standing) = standings.get(&[7; 32]) else {
        return false;
    };
    let Some(subject) = claimed_subject(invoker, &standing.person) else {
        return false;
    };
    let standing = match subject {
        Claimed::Person => standing.clone(),
        Claimed::Through(subject) => standing.through(subject),
    };
    standing.answer(&TreePath::room(ROOM), ActionKind::Undoable, &Context::at(0)) == Answer::Allow
}

fn agent() -> ActionInvoker {
    ActionInvoker::actor("build")
        .with_kind(ActorKind::Agent)
        .acting_for("ada")
}

#[test]
fn a_person_edits_the_room_their_standing_is_about() {
    assert!(allows(&standings(), None));
    assert!(allows(&standings(), Some(&ActionInvoker::actor("ada"))));
}

#[test]
fn an_agent_edits_what_its_delegation_grants() {
    let rooms = standings();
    let standing = rooms.get(&[7; 32]).unwrap();
    let bounded = standing.through(Subject::Agent("build".to_owned()));

    // The delegation is bounded by what the person holds in the room (their
    // own party's grants, plus the delegation's own) and by everything
    // bounding them already.
    let authority = bounded.authority_at(&Context::at(0));
    let last = authority.bounds.last().unwrap();
    assert_eq!(last.subjects, vec![Subject::Agent("build".to_owned())]);
    assert!(last.grants.len() > 20);
    assert!(allows(&rooms, Some(&agent())));
}

#[test]
fn an_agent_cannot_do_what_its_delegation_does_not_grant() {
    let standings = standings();
    let standing = standings.get(&[7; 32]).unwrap();
    let bounded = standing.through(Subject::Agent("build".to_owned()));

    // The agent preset stops at undoable changes: reaching outside the room
    // (posting to someone's mailbox) asks the person, and an agent posting
    // for them is not the person answering.
    assert_ne!(
        bounded.answer(&TreePath::room(ROOM), ActionKind::Outside, &Context::at(0)),
        Answer::Allow
    );
    // Ada, doing the same thing herself, is asked: the grant is hers.
    assert_eq!(
        standing.answer(&TreePath::room(ROOM), ActionKind::Outside, &Context::at(0)),
        Answer::Ask
    );
}

#[test]
fn an_agent_of_no_delegation_edits_nothing() {
    let standings = standings();
    let standing = standings.get(&[7; 32]).unwrap();
    let stranger = standing.through(Subject::Agent("unheard-of".to_owned()));
    assert_ne!(
        stranger.answer(&TreePath::room(ROOM), ActionKind::Undoable, &Context::at(0)),
        Answer::Allow
    );
}

#[test]
fn a_change_claiming_someone_elses_authority_is_decided_by_no_standing() {
    let for_grace = ActionInvoker::actor("build")
        .with_kind(ActorKind::Agent)
        .acting_for("grace");
    assert!(claimed_subject(Some(&for_grace), "ada").is_none());
    assert!(!allows(&standings(), Some(&for_grace)));
}

#[test]
fn an_agent_naming_no_person_keeps_its_own_subject() {
    // A delegation is the device's, so a change may name the agent alone;
    // the standing it is decided by is still bounded by the delegation.
    let own = ActionInvoker::actor("build").with_kind(ActorKind::Agent);
    assert_eq!(
        claimed_subject(Some(&own), "ada"),
        Some(Claimed::Through(Subject::Agent("build".to_owned())))
    );
    assert!(allows(&standings(), Some(&own)));
}

#[test]
fn a_bot_is_bounded_by_its_installation() {
    let bot = ActionInvoker::actor("7e1c")
        .with_kind(ActorKind::Bot)
        .acting_for("ada");
    assert_eq!(
        claimed_subject(Some(&bot), "ada"),
        Some(Claimed::Through(Subject::Bot("7e1c".to_owned())))
    );
    // Nothing in the room grants that installation, so it is bounded away.
    assert!(!allows(&standings(), Some(&bot)));
}

#[test]
fn a_shortcut_or_app_does_not_narrow_the_person() {
    for kind in [ActorKind::Shortcut, ActorKind::App] {
        let invoker = ActionInvoker::actor("assistant")
            .with_kind(kind)
            .acting_for("ada");
        assert_eq!(
            claimed_subject(Some(&invoker), "ada"),
            Some(Claimed::Person)
        );
    }
}
