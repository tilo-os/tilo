//! A record kept to its readers: one Deny for everyone but them.

use tilo_action_policy::{
    ActionKind, Answer, Authority, Context, Role, RoomPolicy, Subject, TreePath, roles,
};

const NOW: u64 = 1_000_000;
const OWNER: &str = "0e4e";
const ADA: &str = "ada";
const BEA: &str = "bea";
const CAL: &str = "cal";

fn cx() -> Context<'static> {
    Context::at(NOW)
}

fn record() -> TreePath {
    TreePath::room("r").app("tasks").resource("secret")
}

fn other_record() -> TreePath {
    TreePath::room("r").app("tasks").resource("tracker")
}

/// Ada an Editor, Bea a Viewer, Cal an Editor; the record kept to Ada, Bea
/// and the agent `scout`.
fn room() -> RoomPolicy {
    let mut room = RoomPolicy::new("r", OWNER);
    room.members.insert(ADA.into(), Role::Editor);
    room.members.insert(BEA.into(), Role::Viewer);
    room.members.insert(CAL.into(), Role::Editor);
    room.grants.push(roles::readers_grant(
        "r",
        "tasks",
        "secret",
        [
            Subject::person(ADA),
            Subject::person(BEA),
            Subject::agent("scout"),
        ],
        Subject::person(OWNER),
    ));
    room
}

fn person(room: &RoomPolicy, who: &str, node: &TreePath, kind: ActionKind) -> Answer {
    Authority::person(room.party(who))
        .decide(node, kind, &cx())
        .answer
}

fn agent(room: &RoomPolicy, agent: &str, person: &str, node: &TreePath) -> Answer {
    let delegation = roles::agent_delegation("r", agent, person);
    Authority::person(room.party(person))
        .bounded_by(delegation.party(&room.grants))
        .decide(node, ActionKind::Read, &cx())
        .answer
}

#[test]
fn only_its_readers_and_the_owner_read_a_kept_record() {
    let room = room();
    assert_eq!(
        person(&room, ADA, &record(), ActionKind::Read),
        Answer::Allow
    );
    assert_eq!(
        person(&room, BEA, &record(), ActionKind::Read),
        Answer::Allow
    );
    assert_eq!(
        person(&room, CAL, &record(), ActionKind::Read),
        Answer::Deny
    );
    assert_eq!(
        person(&room, OWNER, &record(), ActionKind::Read),
        Answer::Allow
    );
    // Everything else of the room reads as before.
    assert_eq!(
        person(&room, CAL, &other_record(), ActionKind::Read),
        Answer::Allow
    );
    // So does someone who joins later: they are not among its readers.
    let mut later = room;
    later.members.insert("dee".into(), Role::Editor);
    assert_eq!(
        person(&later, "dee", &record(), ActionKind::Read),
        Answer::Deny
    );
}

#[test]
fn a_reader_keeps_what_their_role_gives_them_there() {
    let room = room();
    assert_eq!(
        person(&room, ADA, &record(), ActionKind::Undoable),
        Answer::Allow
    );
    // A Viewer reads it and changes nothing, as anywhere in the room.
    assert_eq!(
        person(&room, BEA, &record(), ActionKind::Undoable),
        Answer::Deny
    );
    assert_eq!(
        person(&room, CAL, &record().action("edit"), ActionKind::Undoable),
        Answer::Deny
    );
}

#[test]
fn an_agent_reads_it_only_when_it_and_its_person_are_readers() {
    let room = room();
    assert_eq!(agent(&room, "scout", ADA, &record()), Answer::Allow);
    // Acting for a reader does not make an agent one.
    assert_eq!(agent(&room, "other", ADA, &record()), Answer::Deny);
    // Nor does being named reach past its person.
    assert_eq!(agent(&room, "scout", CAL, &record()), Answer::Deny);
}

#[test]
fn a_reader_is_named_by_person_role_or_agent_and_never_a_bare_delegation() {
    let readers = Subject::all_but([Subject::person(ADA), Subject::role(Role::Moderator)]);
    assert!(!readers.names(&[Subject::person(ADA), Subject::role(Role::Editor)]));
    assert!(!readers.names(&[Subject::person(CAL), Subject::role(Role::Moderator)]));
    assert!(readers.names(&[Subject::person(CAL), Subject::role(Role::Editor)]));
    assert!(readers.names(&[Subject::agent("scout")]));
    // A task, an app or a foreign device acts for a person, who is decided
    // on their own.
    assert!(!readers.names(&[Subject::task("t")]));
    assert!(!readers.names(&[Subject::app("so.tilo.mail")]));
    assert!(!readers.names(&[Subject::ForeignDevice]));
}

#[test]
fn naming_its_readers_again_replaces_the_grant() {
    let first = roles::readers_grant("r", "tasks", "secret", [], Subject::person(OWNER));
    let second = roles::readers_grant(
        "r",
        "tasks",
        "secret",
        [Subject::person(ADA)],
        Subject::person(OWNER),
    );
    assert_eq!(first.id, second.id);
    assert_eq!(
        second.subject.to_string(),
        format!("everyone but {}", Subject::person(ADA))
    );
}
