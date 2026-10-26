//! A delegation acts for one person and never does more than they do. The
//! agent's grant set is what it may do of its own: reading and undoable
//! changes allowed, outside effects asking first, and nothing where the
//! person holds nothing.

use tilo_action_policy::{
    ActionKind, Answer, Authority, Context, Grant, KindSet, Lifetime, Party, Role, RoomPolicy,
    Subject, TreePath, roles,
};

const ALICE: &str = "a11ce";
const BOB: &str = "b0b";
const AGENT: &str = "scribe";
const ISSUE: &str = "tilo/issue/1";

fn cx() -> Context<'static> {
    Context::at(1_000)
}

fn mail() -> TreePath {
    TreePath::room("r").app("mail")
}

fn archive() -> TreePath {
    mail().kind("mail.message").resource("m1").action("archive")
}

fn room_with(members: &[(&str, Role)]) -> RoomPolicy {
    let mut room = RoomPolicy::new("r", ALICE);
    for (person, role) in members {
        room.members.insert((*person).to_owned(), *role);
    }
    room
}

/// The agent's default grant set in Mail, given by `person`.
fn agent_grants(person: &str) -> Vec<Grant> {
    let scope = [mail()];
    roles::agent_default(&scope).assign(
        "delegation",
        &Subject::agent(AGENT),
        &Subject::person(person),
        &Lifetime::UntilRevoked,
    )
}

/// A grant to `subject` at `node`.
fn given(id: &str, subject: Subject, node: TreePath, kinds: KindSet, answer: Answer) -> Grant {
    Grant::new(id, subject, node, kinds, answer, Subject::person(BOB))
}

/// Bob and his agent deciding together, over the room's grants and `extra`.
fn acting(room: &RoomPolicy, extra: &[Grant]) -> Authority<'static> {
    let person = vec![Subject::person(BOB), Subject::agent(AGENT)];
    let grants: Vec<Grant> = room
        .grants
        .iter()
        .cloned()
        .chain(extra.iter().cloned())
        .collect();
    let grants: &'static [Grant] = Box::leak(grants.into_boxed_slice());
    Authority::person(Party::new(person.clone(), grants)).bounded_by(Party::new(person, grants))
}

#[test]
fn a_delegation_never_does_more_than_its_person() {
    // A viewer gave their agent a change of one resource directly. Bob has
    // read the whole of Mail and nothing else, so the delegation may not
    // change anything outside it, however specific its own grant.
    let room = room_with(&[(BOB, Role::Viewer)]);
    let message = mail().kind("mail.message").resource("m1");
    let authority = acting(
        &room,
        &[given(
            "agent-changes-one",
            Subject::agent(AGENT),
            message,
            KindSet::UNDOABLE,
            Answer::Allow,
        )],
    );
    // The agent's own part of the answer, read outside Mail.
    let outside = TreePath::room("r").app("notes").action("append");
    assert_ne!(
        authority
            .person
            .decide(&outside, ActionKind::Undoable, &cx())
            .answer,
        Answer::Allow
    );
    assert_eq!(
        authority
            .decide(&outside, ActionKind::Undoable, &cx())
            .answer,
        Answer::Deny,
        "the person's denial reaches their agent"
    );
}

#[test]
fn the_person_and_their_delegation_are_two_answers() {
    // The person's own grants answer first, then everything the delegation
    // carries taken together: an agent and an issue are one part, not one
    // bound each.
    let room = room_with(&[(BOB, Role::Editor)]);
    let verdict = acting(&room, &agent_grants(BOB)).decide(&archive(), ActionKind::Undoable, &cx());
    assert_eq!(verdict.parts.len(), 2);
    assert_eq!(verdict.parts[0].answer, Answer::Allow);

    let grants: Vec<Grant> = room
        .grants
        .iter()
        .cloned()
        .chain(agent_grants(BOB))
        .collect();
    let grants: &'static [Grant] = Box::leak(grants.into_boxed_slice());
    let run = Authority::person(Party::new(vec![Subject::person(BOB)], grants)).bounded_by(
        Party::new(vec![Subject::agent(AGENT), Subject::task(ISSUE)], grants),
    );
    let verdict = run.decide(&archive(), ActionKind::Undoable, &cx());
    assert_eq!(verdict.parts.len(), 2, "the agent and the issue are one");
}

#[test]
fn a_delegation_with_no_grants_of_its_own_does_nothing() {
    // Bob may change Mail; an agent he delegated nothing to holds nothing.
    let room = room_with(&[(BOB, Role::Editor)]);
    let bob = Authority::person(room.party(BOB));
    let inherited =
        Authority::person(room.party(BOB)).bounded_by(room.party_of(Subject::agent(AGENT)));
    assert_eq!(
        bob.decide(&archive(), ActionKind::Undoable, &cx()).answer,
        Answer::Allow
    );
    assert_eq!(
        inherited
            .decide(&archive(), ActionKind::Undoable, &cx())
            .answer,
        Answer::Deny
    );
}

#[test]
fn a_task_grant_is_spent_with_its_session() {
    // A grant written for one run of an issue is gone from the next one: a
    // run's authority ends with the run.
    let room = room_with(&[(BOB, Role::Editor)]);
    let scoped = given(
        "issue-session",
        Subject::task(ISSUE),
        mail(),
        KindSet::UNDOABLE,
        Answer::Allow,
    )
    .with_lifetime(Lifetime::Session("run-1".to_owned()));
    let grants: Vec<Grant> = room.grants.iter().cloned().chain([scoped]).collect();
    let grants: &'static [Grant] = Box::leak(grants.into_boxed_slice());
    let run = Authority::person(Party::new(vec![Subject::person(BOB)], grants))
        .bounded_by(Party::new(vec![Subject::task(ISSUE)], grants));
    let first = run.decide(&archive(), ActionKind::Undoable, &cx().in_session("run-1"));
    assert_eq!(first.parts.len(), 2);
    assert_eq!(first.parts[1].answer, Answer::Allow, "the run's own grant");
    assert_eq!(first.answer, Answer::Deny, "Bob's role begins at the root");
    assert_eq!(
        run.decide(&archive(), ActionKind::Undoable, &cx().in_session("run-2"))
            .answer,
        Answer::Deny,
        "the next run of the issue holds nothing the earlier one was given"
    );
}
