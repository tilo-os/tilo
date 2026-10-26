//! Every door asks one function: the person's authority, bounded by each
//! delegation, answered as admitted, ask or refused. Discovery asks the same
//! one, so a catalogue never lists what invoking refuses.

use tilo_action_policy::{
    ActionKind, Answer, Bound, Caller, CatalogueEntry, Context, DoorAnswer, Grant, GrantId,
    KindSet, Level, Lifetime, Role, RoomPolicy, Subject, TreePath, admit_catalogue, roles,
};

const NOW: u64 = 1_000_000;
const ROOM: &str = "r";
const OWNER: &str = "a11ce";
const MEMBER: &str = "b0b";

fn cx() -> Context<'static> {
    Context::at(NOW)
}

/// The node a door is asked about, relative to the room, as an app's
/// published tree names it.
fn post() -> TreePath {
    TreePath::room(ROOM)
        .app("so.tilo.chat")
        .kind("chat.workspace")
        .any(Level::Resource)
        .action("post")
}

/// A room where `MEMBER` is a `role`, with the presets set aside: a test's
/// own grant is what decides.
fn room_with(role: Role) -> RoomPolicy {
    let mut room = RoomPolicy::new(ROOM, OWNER);
    room.grants.clear();
    room.members.insert(MEMBER.to_owned(), role);
    room
}

fn policy_granting(subject: Subject, node: &TreePath, answer: Answer) -> RoomPolicy {
    let mut room = room_with(Role::Editor);
    room.grants.insert(
        0,
        Grant::new(
            "test",
            subject,
            TreePath::room(ROOM).joined(node),
            KindSet::ALL,
            answer,
            Subject::person(OWNER),
        ),
    );
    room
}

#[test]
fn a_persons_own_gesture_answers_their_own_ask() {
    let policy = policy_granting(Subject::person(MEMBER), &post(), Answer::Ask);
    let asked = Caller::person(MEMBER).in_room(&policy);
    assert!(
        asked
            .admit(&post(), ActionKind::Undoable, false, false, &cx())
            .is_admitted(),
        "a person acting themselves is the answer to their ask"
    );
}

/// A delegation whose preset allows `node`: what the tests below need for a
/// bound that is not itself the reason an answer refuses.
fn delegation_at(node: &TreePath) -> Bound {
    let agent = Subject::agent("agent-1");
    Bound {
        subject: agent.clone(),
        grants: vec![Grant::new(
            "delegated",
            agent,
            TreePath::room(ROOM).joined(node),
            KindSet::ALL,
            Answer::Allow,
            Subject::person(MEMBER),
        )],
    }
}

#[test]
fn a_delegates_ask_waits_for_the_person() {
    let policy = policy_granting(Subject::person(MEMBER), &post(), Answer::Ask);
    let agent = Caller::person(MEMBER).bounded_by(delegation_at(&post()));
    let asked = agent.in_room(&policy);
    let admission = asked.admit(&post(), ActionKind::Undoable, false, false, &cx());
    assert!(
        admission.is_ask(),
        "an agent's ask is the person's to answer, not its own"
    );
    assert!(
        admission.spent().is_empty(),
        "an ask that did not run spends no grant"
    );
}

/// The person's host on their own device answers their ask, putting it to
/// them: their own gesture is not the only way an ask is answered.
#[test]
fn a_host_witnessed_ask_runs_once_the_person_answers() {
    let policy = policy_granting(Subject::person(MEMBER), &post(), Answer::Ask);
    let witnessed = Caller::person(MEMBER).hosted().in_room(&policy);
    assert!(
        witnessed
            .admit(&post(), ActionKind::Undoable, false, false, &cx())
            .is_admitted(),
        "the host asks them and hands the answered invocation on"
    );
    let unwitnessed = Caller::person(MEMBER).in_room(&policy);
    assert!(
        !unwitnessed
            .admit(&post(), ActionKind::Undoable, false, false, &cx())
            .is_ask(),
        "on their own device the person's gesture is the answer"
    );
}

/// A door that cannot reach the person does not hold their ask: it refuses.
#[test]
fn an_ask_nobody_can_answer_refuses() {
    let policy = policy_granting(Subject::person(MEMBER), &post(), Answer::Ask);
    let asking = Caller::person(MEMBER).bounded_by(delegation_at(&post()));
    assert!(
        asking
            .in_room(&policy)
            .admit(&post(), ActionKind::Undoable, false, false, &cx())
            .is_ask(),
        "a delegation's host puts the ask to its person"
    );
    let alone = asking.unreachable().in_room(&policy);
    let refused = alone.admit(&post(), ActionKind::Undoable, false, false, &cx());
    assert!(
        matches!(refused, DoorAnswer::Refused(_)),
        "nobody is there to answer, so it does not run"
    );
    assert_eq!(refused.answer(), Answer::Deny);
}

/// A record with no grant at all is refused outright, ask or not.
#[test]
fn no_grant_is_refused_rather_than_asked() {
    let policy = room_with(Role::Editor);
    let stranger = Caller::person("c0ffee").in_room(&policy);
    assert_eq!(
        stranger
            .admit(&post(), ActionKind::Undoable, false, false, &cx())
            .answer(),
        Answer::Deny
    );
}

#[test]
fn an_attended_action_asks_a_delegate_even_when_its_grants_allow() {
    let policy = policy_granting(Subject::person(MEMBER), &post(), Answer::Allow);
    let agent = Caller::person(MEMBER).bounded_by(delegation_at(&post()));
    let asked = agent.in_room(&policy);
    assert!(
        asked
            .admit(&post(), ActionKind::Undoable, false, true, &cx())
            .is_ask(),
        "an attended action waits for its person"
    );
    assert!(
        asked
            .admit(&post(), ActionKind::Undoable, false, false, &cx())
            .is_admitted(),
        "what does not wait for them runs on its grants"
    );
}

#[test]
fn a_person_on_a_foreign_device_answers_their_ask_through_their_host() {
    let policy = policy_granting(Subject::person(MEMBER), &post(), Answer::Ask);
    let away = Caller::person(MEMBER)
        .bounded_by(delegation_at(&post()))
        .on_foreign_device()
        .in_room(&policy);
    assert!(
        away.admit(&post(), ActionKind::Undoable, false, false, &cx())
            .is_ask(),
        "a device that is not theirs cannot answer for them"
    );
}

#[test]
fn a_delegation_never_reaches_past_its_person() {
    let policy = policy_granting(Subject::person(MEMBER), &TreePath::room(ROOM), Answer::Deny);
    let agent = Caller::person(MEMBER).bounded_by(roles::agent_delegation(ROOM, "agent-1", MEMBER));
    assert_eq!(
        agent
            .in_room(&policy)
            .admit(&post(), ActionKind::Undoable, false, false, &cx())
            .answer(),
        Answer::Deny,
        "the person's deny bounds the delegation"
    );
}

#[test]
fn a_privileged_action_needs_the_persons_own_allow() {
    let policy = policy_granting(Subject::person(MEMBER), &post(), Answer::Ask);
    assert_eq!(
        Caller::person(MEMBER)
            .in_room(&policy)
            .admit(&post(), ActionKind::Outside, true, false, &cx())
            .answer(),
        Answer::Deny,
        "an ask is no authority the person lacks, so it cannot stand for one"
    );
}

#[test]
fn discovery_marks_exactly_what_the_door_answers() {
    let allowed = post();
    let governed = roles::governance_action(ROOM, roles::governance::ALL[0]);
    let mut policy = policy_granting(Subject::person(MEMBER), &allowed, Answer::Ask);
    policy.grants.insert(
        0,
        Grant::new(
            "governed",
            Subject::person(MEMBER),
            governed.clone(),
            KindSet::ALL,
            Answer::Deny,
            Subject::person(MEMBER),
        ),
    );
    let asked = Caller::person(MEMBER).in_room(&policy);
    let entries = [
        CatalogueEntry {
            node: &allowed,
            kind: ActionKind::Undoable,
        },
        CatalogueEntry {
            node: &governed,
            kind: roles::GOVERNANCE_KIND,
        },
    ];
    let marks = admit_catalogue(&asked, entries.iter().copied(), &cx());
    assert_eq!(marks, vec![Answer::Ask, Answer::Deny]);
    for (entry, mark) in entries.iter().zip(marks) {
        assert_eq!(
            asked
                .admit(entry.node, entry.kind, false, false, &cx())
                .answer(),
            mark,
            "a listing and the door agree on {}",
            entry.node
        );
    }
}

#[test]
fn an_admitted_run_spends_its_once_grant_and_a_refused_one_keeps_it() {
    let mut once = policy_granting(Subject::person(MEMBER), &post(), Answer::Allow);
    once.grants.insert(
        0,
        Grant::new(
            "once",
            Subject::person(MEMBER),
            TreePath::room(ROOM).joined(&post()),
            KindSet::ALL,
            Answer::Allow,
            Subject::person(OWNER),
        ),
    );
    if let Some(grant) = once.grants.first_mut() {
        grant.lifetime = Lifetime::Once;
    }
    let asked = Caller::person(MEMBER).in_room(&once);
    let admission = asked.admit(&post(), ActionKind::Undoable, false, false, &cx());
    assert!(admission.is_admitted());
    assert_eq!(
        admission.spent(),
        vec![GrantId::new("once")],
        "the door hands the run's `Once` grant on to be revoked"
    );

    // A privileged action on another app's node: nothing grants it, and no
    // ask could stand in for the person's own allow.
    let elsewhere = TreePath::room(ROOM).app("so.tilo.notes").action("erase");
    let refused = asked.admit(&elsewhere, ActionKind::Outside, true, false, &cx());
    assert!(matches!(refused, DoorAnswer::Refused(_)));
    assert!(refused.spent().is_empty(), "a refused run spends nothing");
    assert_eq!(refused.answer(), Answer::Deny);
}

#[test]
fn a_non_member_is_refused_by_the_same_door() {
    let policy = room_with(Role::Editor);
    let stranger = Caller::person("c0ffee").in_room(&policy);
    assert_eq!(
        stranger
            .admit(&post(), ActionKind::Read, false, false, &cx())
            .answer(),
        Answer::Deny
    );
}
