//! A room's administrator: an Editor the owner gave every governance action.
//! They govern, an Editor by role alone does not, and their role, a foreign
//! device, an agent, a ban and a restriction still bound them.

use tilo_action_policy::{
    ActionKind, Answer, Authority, Change, Context, Grant, KindSet, Lifetime, Refusal, Role,
    RoomPolicy, Subject, TreePath,
    roles::{self, governance},
};

const NOW: u64 = 1_000_000;
const OWNER: &str = "a11ce";
const BOB: &str = "b0b";
const CAROL: &str = "ca401";

fn cx() -> Context<'static> {
    Context::at(NOW)
}

/// Room `r` owned by [`OWNER`] with `members`, and `administrator` made its
/// administrator by the owner.
fn room(members: &[(&str, Role)], administrator: &str) -> RoomPolicy {
    let mut room = RoomPolicy::new("r", OWNER);
    for (person, role) in members {
        room.members.insert((*person).to_owned(), *role);
    }
    room.grants.push(roles::administrator_grant(
        "r",
        administrator,
        Subject::person(OWNER),
    ));
    room
}

#[test]
fn an_administrator_runs_every_governance_action_and_an_editor_none() {
    let room = room(&[(BOB, Role::Editor), (CAROL, Role::Editor)], CAROL);
    let bob = Authority::person(room.party(BOB));
    let carol = Authority::person(room.party(CAROL));
    for action in governance::ALL {
        assert_eq!(
            room.check_action(&carol, action, &cx(), false),
            Ok(()),
            "an administrator runs {action}"
        );
        assert!(
            matches!(
                room.check_action(&bob, action, &cx(), true),
                Err(Refusal::NotPermitted(_))
            ),
            "an Editor by role alone does not run {action}"
        );
    }
    // The grant reaches governance only: content stays the Editor's.
    let send = TreePath::room("r")
        .app("mail")
        .kind("mail.message")
        .resource("m1")
        .action("send");
    assert_eq!(
        carol.decide(&send, ActionKind::Outside, &cx()).answer,
        Answer::Ask
    );
}

#[test]
fn an_administrator_never_hands_out_or_reaches_above_their_role() {
    let room = room(
        &[
            (BOB, Role::Editor),
            (CAROL, Role::Editor),
            ("m", Role::Moderator),
            ("v", Role::Viewer),
        ],
        CAROL,
    );
    let carol = Authority::person(room.party(CAROL));
    room.check_invite(CAROL, &carol, Role::Editor, &cx(), false)
        .unwrap();
    assert_eq!(
        room.check_invite(CAROL, &carol, Role::Moderator, &cx(), false),
        Err(Refusal::AboveActorRole {
            actor: Some(Role::Editor),
            wanted: Role::Moderator
        })
    );
    assert_eq!(
        room.check_invite(CAROL, &carol, Role::Owner, &cx(), false),
        Err(Refusal::OwnerRole)
    );
    let set = |person: &str, role| Change::SetRole {
        person: person.into(),
        role,
    };
    room.check(CAROL, &carol, &set("v", Role::Editor), &cx(), false)
        .unwrap();
    assert!(matches!(
        room.check(CAROL, &carol, &set(BOB, Role::Moderator), &cx(), false),
        Err(Refusal::AboveActorRole { .. })
    ));
    let remove = |person: &str| Change::RemoveMember {
        person: person.into(),
    };
    room.check(CAROL, &carol, &remove("v"), &cx(), false)
        .unwrap();
    for person in [BOB, "m"] {
        assert!(matches!(
            room.check(CAROL, &carol, &remove(person), &cx(), false),
            Err(Refusal::NotBelowActor { .. })
        ));
    }
    assert_eq!(
        room.check(CAROL, &carol, &remove(OWNER), &cx(), false),
        Err(Refusal::Owner)
    );
    // A grant beyond what the administrator holds on its node.
    let wider = Grant::new(
        "wider",
        Subject::person(BOB),
        room.node(),
        KindSet::OUTSIDE,
        Answer::Allow,
        Subject::person(CAROL),
    );
    assert!(matches!(
        room.check(CAROL, &carol, &Change::Grant(wider), &cx(), false),
        Err(Refusal::AboveActorGrant { .. })
    ));
}

#[test]
fn an_administrator_governs_nothing_from_a_foreign_device_or_through_an_agent() {
    let room = room(&[(CAROL, Role::Editor)], CAROL);
    let bound = roles::foreign_device_bound();
    let foreign = room.person_on_device(CAROL, true, &bound);
    let delegation = roles::agent_delegation("r", "scribe", CAROL);
    let agent = Authority::person(room.party(CAROL)).bounded_by(delegation.party(&room.grants));
    for action in governance::ALL {
        assert!(room.check_action(&foreign, action, &cx(), true).is_err());
        assert!(room.check_action(&agent, action, &cx(), true).is_err());
    }
}

#[test]
fn a_ban_or_restriction_stops_whatever_governance_a_member_holds() {
    let until = NOW + 60_000;
    let mut restricted = room(&[(CAROL, Role::Editor), (BOB, Role::Moderator)], CAROL);
    for person in [CAROL, BOB] {
        restricted.grants.push(roles::restriction_grant(
            "r",
            person,
            until,
            Subject::person(OWNER),
        ));
    }
    let carol = Authority::person(restricted.party(CAROL));
    let bob = Authority::person(restricted.party(BOB));
    assert_eq!(
        restricted.check_action(&carol, governance::INVITE, &cx(), true),
        Err(Refusal::Restricted(CAROL.into()))
    );
    assert_eq!(
        restricted.check_action(&bob, governance::BAN, &cx(), true),
        Err(Refusal::Restricted(BOB.into())),
        "a Moderator's own actions do not outlast a restriction either"
    );
    assert_eq!(
        restricted.check_action(&carol, governance::INVITE, &Context::at(until), false),
        Ok(()),
        "a lapsed restriction holds nobody off"
    );

    let mut banned = room(&[(CAROL, Role::Editor)], CAROL);
    banned
        .grants
        .push(roles::block_grant("r", CAROL, Subject::person(OWNER)));
    let carol = Authority::person(banned.party(CAROL));
    assert_eq!(
        banned.check_action(&carol, governance::INVITE, &cx(), true),
        Err(Refusal::Banned(CAROL.into()))
    );
    let owner = Authority::person(banned.party(OWNER));
    assert_eq!(
        banned.check_action(&owner, governance::INVITE, &cx(), false),
        Ok(())
    );
}

#[test]
fn an_administrator_grant_on_the_rooms_resource_is_outranked_by_the_editor_rule() {
    let mut room = RoomPolicy::new("r", OWNER);
    room.members.insert(CAROL.into(), Role::Editor);
    let legacy = Grant::new(
        roles::administrator_grant_id(CAROL).as_str(),
        Subject::person(CAROL),
        roles::room_resource("r"),
        KindSet::ALL,
        Answer::Allow,
        Subject::person(OWNER),
    );
    assert!(roles::is_legacy_administrator_grant("r", &legacy));
    assert!(!roles::is_legacy_administrator_grant(
        "r",
        &roles::administrator_grant("r", CAROL, Subject::person(OWNER))
    ));
    let denied = Grant {
        answer: Answer::Deny,
        ..legacy.clone()
    };
    let expiring = Grant {
        lifetime: Lifetime::Until(NOW + 1),
        ..legacy.clone()
    };
    for other in [denied, expiring] {
        assert!(
            !roles::is_legacy_administrator_grant("r", &other),
            "rewriting {other:?} would widen it"
        );
    }
    room.grants.push(legacy);
    let carol = Authority::person(room.party(CAROL));
    assert!(matches!(
        room.check_action(&carol, governance::INVITE, &cx(), false),
        Err(Refusal::NotPermitted(_))
    ));
}

/// A task's delegation never governs the room it runs in. The room may hold a
/// grant for the task by name — an owner may even allow it a governance verb —
/// and a run of it reads that grant, but whoever runs the task also holds the
/// task's delegation, which is the person who started it. A person the room's
/// governance does not admit therefore leaves the task unable to govern,
/// whoever granted the task what.
#[test]
fn a_task_with_a_governance_grant_still_asks_its_person_before_governing() {
    // The owner is the room's only administrator: an Editor may not govern.
    let mut room = room(&[(BOB, Role::Editor)], OWNER);
    let task = "tasks.tracker/issues/7";
    for (id, action) in [
        ("issue-governs", governance::INVITE),
        ("issue-removes", governance::REMOVE_MEMBER),
    ] {
        room.grants.push(Grant::new(
            id,
            Subject::Task(task.to_owned()),
            roles::governance_action("r", action),
            KindSet::of(roles::GOVERNANCE_KIND),
            Answer::Allow,
            Subject::person(OWNER),
        ));
    }
    let task_bound = roles::agent_policy::launch_bound("r", Subject::Task(task.to_owned()), BOB);
    let starter = Authority::person(room.party(BOB)).bounded_by(task_bound.party(&room.grants));

    for action in governance::ALL {
        for approved in [false, true] {
            assert!(
                matches!(
                    room.check_action(&starter, action, &cx(), approved),
                    Err(Refusal::NotPermitted(_))
                ),
                "a task whatever the room granted it never governs through {action}"
            );
        }
    }
    for held in ["issue-governs", "issue-removes"] {
        let grant = room
            .grants
            .iter()
            .find(|grant| grant.id.as_str() == held)
            .expect("the grant is in the room");
        assert!(
            RoomPolicy::check_within(&starter, grant, &cx()).is_err(),
            "{held} reaches past the person who started the task"
        );
    }

    // A person the room's governance does admit hands their task that reach.
    let owner = Authority::person(room.party(OWNER));
    for held in ["issue-governs", "issue-removes"] {
        let grant = room
            .grants
            .iter()
            .find(|grant| grant.id.as_str() == held)
            .expect("the grant is in the room");
        assert_eq!(
            RoomPolicy::check_within(&owner, grant, &cx()),
            Ok(()),
            "the owner grants {held} without asking anyone"
        );
    }
}
