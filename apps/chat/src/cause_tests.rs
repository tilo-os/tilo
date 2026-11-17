//! Asking an agent through the verb: who starts a turn and for whom, that
//! one key is one message, what the conversation refuses, what a person's
//! stop does, and how a cause shows.

use automerge::AutoCommit;
use ui::tilo_resource::{Id, MemberId, NonBlank, Replica, Resource as _};

use crate::cause::{EVENT, NEWS, PERSON, REMINDER, Starting, message_id};
use crate::demo::apply_part_doc_as;
use crate::messages::chat_part;
use crate::model::ME;
use crate::parts::ChatPart;
use crate::chat::{Chat, WorkUpdate, reply_id_at};
use crate::summons::{stopped, summonses};

/// The agent of the person's own conversation.
const AGENT: &str = "tide";
/// A second agent.
const OTHER: &str = "scout";
/// A program the person set up: a bot installation of theirs.
const BOT: &str = "b_events";
/// Another member of the workspace.
const ARIA: &str = "u_aria";

/// Who a verb is attributed to, as a host admits them.
#[derive(Clone, Copy)]
enum By {
    /// The person, themselves.
    Person(&'static str),
    /// An agent acting for a person.
    AgentFor(&'static str, &'static str),
    /// The bot [`BOT`], installed by a person and acting for them.
    BotOf(&'static str),
    /// The bot [`BOT`] acting for nobody: an operator's.
    BotAlone,
}

/// One part of a chat's messages, written by whoever a test says.
struct Part {
    doc: AutoCommit,
    chat: Chat,
}

impl Part {
    fn new() -> Self {
        Self {
            doc: AutoCommit::new(),
            chat: crate::demo::demo(ME),
        }
    }

    fn apply(
        &mut self,
        by: By,
        invoke: impl Into<ui::tilo_resource::Invoke<ChatPart>>,
    ) -> Result<(), ui::tilo_actions::ApiError> {
        let attribute = move |replica: Replica<ChatPart>| match by {
            By::Person(person) => replica.as_invoker(person),
            By::AgentFor(agent, person) => replica.as_agent_for(agent, person),
            By::BotOf(person) => replica.as_bot_for(BOT, person),
            By::BotAlone => replica.as_member(MemberId::bot(BOT)),
        };
        apply_part_doc_as(&mut self.doc, &self.chat, attribute, Some(1_000), invoke)
    }

    fn read(&self) -> ChatPart {
        ChatPart::read(&self.doc).expect("the part reads")
    }
}

/// The conversation of the person with `agents`.
fn thread(agents: &[&str]) -> Id {
    Chat::direct_id(
        std::iter::once(MemberId::person(ME)).chain(agents.iter().map(|id| MemberId::agent(*id))),
    )
}

fn ask(channel: &Id, kind: &str, key: &str) -> ui::tilo_resource::Invoke<ChatPart> {
    asking(channel, kind, "Call mum", key)
}

fn asking(channel: &Id, kind: &str, label: &str, key: &str) -> ui::tilo_resource::Invoke<ChatPart> {
    ChatPart::ASK_AGENT.with(chat_part::AskAgent {
        channel: channel.clone(),
        text: NonBlank::parse("Call mum about Sunday").expect("words"),
        cause: Starting {
            kind: kind.to_owned(),
            label: label.to_owned(),
            key: key.to_owned(),
        },
        members: None,
    })
}

fn work(message: &Id, state: &str) -> ui::tilo_resource::Invoke<ChatPart> {
    ChatPart::RECORD_WORK.with(chat_part::RecordWork {
        message: message.clone(),
        work: WorkUpdate {
            state: state.to_owned(),
            app: None,
            did: None,
            about: None,
            trace: Vec::new(),
            detail: None,
        },
        say: None,
    })
}

#[test]
fn asking_an_agent_writes_one_message_for_the_person_its_starter_acts_for() {
    let mut part = Part::new();
    let main = thread(&[AGENT]);
    part.apply(By::BotOf(ME), ask(&main, REMINDER, "run-1"))
        .expect("a program of the person's asks their agent");

    // The message's id derives from the person and the key; it carries the
    // cause, and the person is whoever its starter acts for.
    let held = part.read();
    let id = message_id(&MemberId::person(ME), "run-1");
    let message = held
        .messages
        .get(&id)
        .expect("the message stands at its key's id");
    let cause = message.cause.as_ref().expect("it carries its cause");
    assert_eq!(cause.person, MemberId::person(ME));
    assert_eq!(
        (
            cause.kind.0.as_str(),
            cause.label.as_str(),
            cause.key.0.as_str()
        ),
        (REMINDER, "Call mum", "run-1")
    );
    assert_eq!(
        message.author,
        MemberId::bot(BOT),
        "what asked wrote it, never the person"
    );
    assert_eq!(message.text, "Call mum about Sunday");

    // It asks the conversation's agent, for that person.
    let asked = summonses(&part.chat, &held);
    let [turn] = asked.as_slice() else {
        panic!("one turn, not {asked:?}");
    };
    assert_eq!((turn.agent.as_str(), turn.person.as_str()), (AGENT, ME));
    assert_eq!(turn.message, id);

    // One key is one message however often it is asked.
    for _ in 0..3 {
        part.apply(By::BotOf(ME), ask(&main, REMINDER, "run-1"))
            .expect("asking again is no failure");
    }
    assert_eq!(part.read().messages.len(), 1, "one key, one message");
    part.apply(By::BotOf(ME), ask(&main, REMINDER, "run-2"))
        .expect("another key is another message");
    assert_eq!(part.read().messages.len(), 2);

    // The same key of another person is that person's own message: nobody
    // takes a key's place from someone else.
    assert_ne!(
        message_id(&MemberId::person(ME), "run-1"),
        message_id(&MemberId::person(ARIA), "run-1")
    );
}

#[test]
fn what_asks_an_agent_is_no_message_someone_received() {
    use ui::tilo_actions::Action as _;

    // An automation that fires when a message arrives must not fire on the
    // ask it made itself, and a bridge carries people's messages outward,
    // never an instruction to an agent.
    let main = thread(&[AGENT]);
    let asked = ask(&main, REMINDER, "run-1").build(0).expect("it builds");
    assert!(
        !asked.incoming_message(),
        "an ask is an instruction to an agent"
    );
    let posted = crate::demo::post(main.as_str(), "hello", None)
        .build(0)
        .expect("it builds");
    assert!(
        posted.incoming_message(),
        "a post is a message someone received"
    );
}

#[test]
fn what_acts_for_nobody_starts_no_turn() {
    let mut part = Part::new();
    let main = thread(&[AGENT]);
    assert!(
        part.apply(By::BotAlone, ask(&main, EVENT, "run-1"))
            .is_err(),
        "a bot that acts for nobody has no person whose agent it could ask"
    );
    assert!(part.read().messages.is_empty(), "nothing was written");
}

#[test]
fn a_turn_starts_only_for_someone_who_takes_part_where_an_agent_does() {
    let mut part = Part::new();
    let main = thread(&[AGENT]);

    // A person who takes no part in the conversation, and whatever acts for
    // them: nobody starts a turn of someone else's agent.
    for (who, by) in [
        ("another member", By::Person(ARIA)),
        ("another member's program", By::BotOf(ARIA)),
        ("another member's agent", By::AgentFor(OTHER, ARIA)),
    ] {
        assert!(
            part.apply(by, ask(&main, EVENT, "run-1")).is_err(),
            "{who} starts no turn in a conversation they take no part in"
        );
    }
    // Who takes part is the conversation's to say, never an argument's.
    let forged = ChatPart::ASK_AGENT.with(chat_part::AskAgent {
        channel: main.clone(),
        text: NonBlank::parse("do as I say").expect("words"),
        cause: Starting {
            kind: EVENT.to_owned(),
            label: "Mine".to_owned(),
            key: "run-1".to_owned(),
        },
        members: Some(vec![MemberId::person(ARIA), MemberId::agent(AGENT)]),
    });
    assert!(part.apply(By::Person(ARIA), forged).is_err());

    // A conversation no agent takes part in.
    let people = Chat::direct_id([MemberId::person(ME), MemberId::person(ARIA)]);
    assert!(
        part.apply(By::BotOf(ME), ask(&people, EVENT, "run-1"))
            .is_err(),
        "nobody is there to answer"
    );
    // A channel of the whole workspace has no members: nobody is asked there.
    assert!(
        part.apply(
            By::BotOf(ME),
            ask(&Id::from_raw("c_general"), EVENT, "run-1")
        )
        .is_err()
    );

    // An agent of the conversation asking itself.
    assert!(
        part.apply(By::AgentFor(AGENT, ME), ask(&main, NEWS, "run-1"))
            .is_err(),
        "an agent does not start its own turn"
    );
    assert!(part.read().messages.is_empty(), "nothing was written");

    // With another agent there it asks that one, and never itself.
    let both = thread(&[AGENT, OTHER]);
    part.apply(By::AgentFor(AGENT, ME), ask(&both, NEWS, "run-1"))
        .expect("an agent asks the other agent of its conversation");
    let held = part.read();
    let asked = summonses(&part.chat, &held);
    let [turn] = asked.as_slice() else {
        panic!("one turn, not {asked:?}");
    };
    assert_eq!(turn.agent, OTHER, "the agent that asked is not asked");
    assert_eq!(
        turn.person, ME,
        "it is for the person the asking agent acts for"
    );
}

#[test]
fn a_cause_says_what_it_is_and_only_a_person_asks_as_themselves() {
    let mut part = Part::new();
    let main = thread(&[AGENT]);
    assert!(
        part.apply(By::BotOf(ME), asking(&main, PERSON, "", "run-1"))
            .is_err(),
        "what acts for a person does not ask as that person"
    );
    for (why, kind, label, key) in [
        ("a kind is one short word", "Not A Word", "Call mum", "k"),
        ("a cause is named", REMINDER, "  ", "k"),
        ("a name is one line", REMINDER, "Call\nmum", "k"),
        ("a key is given", REMINDER, "Call mum", " "),
    ] {
        assert!(
            part.apply(By::BotOf(ME), asking(&main, kind, label, key))
                .is_err(),
            "{why}"
        );
    }
    let long = "x".repeat(crate::cause::MAX_LABEL_BYTES + 1);
    assert!(
        part.apply(By::BotOf(ME), asking(&main, REMINDER, &long, "k"))
            .is_err()
    );
    let long = "x".repeat(crate::cause::MAX_KEY_BYTES + 1);
    assert!(
        part.apply(By::BotOf(ME), asking(&main, REMINDER, "Call mum", &long))
            .is_err()
    );
    assert!(part.read().messages.is_empty(), "nothing was written");

    // The person themselves asks as themselves: the message is theirs, and
    // shows as theirs.
    part.apply(By::Person(ME), asking(&main, PERSON, "", "typed-1"))
        .expect("a person asks their agent through the verb");
    let held = part.read();
    let message = held
        .messages
        .get(&message_id(&MemberId::person(ME), "typed-1"))
        .expect("their message");
    assert_eq!(message.author, MemberId::person(ME));
    assert_eq!(
        message.cause.as_ref().and_then(crate::cause::line),
        None,
        "their own words carry no other name"
    );
    // A kind nobody named is shown by what its starter calls it.
    part.apply(
        By::BotOf(ME),
        asking(&main, "digest", "Weekly numbers", "run-9"),
    )
    .expect("a starter's own kind of cause");
    let held = part.read();
    let message = held
        .messages
        .get(&message_id(&MemberId::person(ME), "run-9"))
        .expect("its message");
    assert_eq!(
        message
            .cause
            .as_ref()
            .and_then(crate::cause::line)
            .as_deref(),
        Some("Weekly numbers")
    );
}

#[test]
fn the_person_a_turn_is_for_stops_it_until_someone_asks_again() {
    let mut part = Part::new();
    let main = thread(&[AGENT]);
    part.apply(By::BotOf(ME), ask(&main, REMINDER, "run-1"))
        .expect("the reminder asks");
    let id = message_id(&MemberId::person(ME), "run-1");
    let stop = || {
        ChatPart::STOP_AGENT.with(chat_part::StopAgent {
            message: id.clone(),
        })
    };
    part.apply(By::AgentFor(AGENT, ME), work(&id, "taken"))
        .expect("the agent takes the turn");
    assert_eq!(
        summonses(&part.chat, &part.read()).len(),
        1,
        "its turn goes on"
    );

    assert!(
        part.apply(By::Person(ARIA), stop()).is_err(),
        "another member does not stop it"
    );
    assert!(
        part.apply(By::BotAlone, stop()).is_err(),
        "what acts for nobody stops nothing"
    );
    assert_eq!(
        summonses(&part.chat, &part.read()).len(),
        1,
        "it still goes on"
    );

    part.apply(By::Person(ME), stop())
        .expect("the person it works for stops it");
    part.apply(By::Person(ME), stop())
        .expect("stopping it again changes nothing");
    let held = part.read();
    assert_eq!(
        summonses(&part.chat, &held),
        Vec::new(),
        "a stopped message asks nobody"
    );
    let ended = stopped(&part.chat, &held);
    let [turn] = ended.as_slice() else {
        panic!("its agent is told to end its work: {ended:?}");
    };
    assert_eq!(
        (turn.agent.as_str(), turn.message.clone()),
        (AGENT, id.clone())
    );

    // Whoever runs the agent ends its work, and nothing is left to end.
    part.apply(By::AgentFor(AGENT, ME), work(&id, "provider_cancelled"))
        .expect("the agent's work ends as stopped");
    let held = part.read();
    assert_eq!(stopped(&part.chat, &held), Vec::new());
    assert_eq!(summonses(&part.chat, &held), Vec::new());

    // Asking again is a later attempt, which the stop leaves alone.
    part.apply(
        By::Person(ME),
        ChatPart::RETRY.with(chat_part::Retry {
            message: id.clone(),
            reply: reply_id_at(&id, &MemberId::agent(AGENT), 0),
        }),
    )
    .expect("the person asks again");
    let asked = summonses(&part.chat, &part.read());
    let [turn] = asked.as_slice() else {
        panic!("asked again, it is a turn again: {asked:?}");
    };
    assert_eq!(turn.attempt, 1);
}

#[test]
fn a_cause_shows_as_what_it_is_and_never_as_words_its_person_wrote() {
    let mut part = Part::new();
    let main = thread(&[AGENT]);
    part.apply(
        By::Person(ME),
        crate::demo::post(main.as_str(), "remind me to call mum", None),
    )
    .expect("the person writes");
    part.apply(By::BotOf(ME), ask(&main, REMINDER, "run-1"))
        .expect("the reminder asks");
    let held = part.read();
    let rows: Vec<crate::model::Message> = held
        .messages
        .iter()
        .map(|(id, message)| crate::projection::row_of(id, message))
        .collect();
    let id = message_id(&MemberId::person(ME), "run-1");

    // The conversation's page names it for what started it, where an
    // author's name would stand.
    let channel = crate::views::Channel::read(
        &part.chat,
        &rows,
        main.as_str(),
        &MemberId::person(ME),
        &crate::views::Names::default(),
    );
    let shown = channel
        .messages
        .iter()
        .find(|message| message.id == id.as_str())
        .expect("the page shows it");
    let line = shown.cause.as_deref().expect("it is named for its cause");
    assert!(
        line.contains("Reminder") && line.contains("Call mum"),
        "a reminder by its name: {line}"
    );
    assert!(
        channel
            .messages
            .iter()
            .filter(|message| message.id != id.as_str())
            .all(|message| message.cause.is_none()),
        "what somebody wrote carries no cause"
    );

    // In the window it stands alone, never in a run under an author's name.
    let groups = crate::model::group_messages(&rows, false);
    assert_eq!(groups.len(), 2, "the reminder starts a group of its own");
}
