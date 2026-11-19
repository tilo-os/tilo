//! A question a message asks: who may ask it and who may answer, how it
//! stands from asked to settled, and what Chat's window draws for it.

use automerge::AutoCommit;
use serde_json::json;
use ui::framework::testing::TestApp;
use ui::tilo_actions::{Acting, ActionInvoker};
use ui::tilo_resource::{Id, MemberId, NonBlank, Replica, Resource as _};

use crate::demo::apply_part_doc_as;
use crate::messages::chat_messages;
use crate::model::ME;
use crate::parts::ChatMessages;
use crate::question::{
    ACTION, ALLOW, ALWAYS, Choice, DENY, DONE, DROPPED, Question, QuestionState, STARTED,
};
use crate::resource::ChatShell;
use crate::testing::{MountedChat, chain, message_item, mounted_rows};

/// The agent that asks.
const AGENT: &str = "tide";
/// A second in milliseconds, and the day a question waits in these tests.
const SECOND_MS: i64 = 1_000;
const DAY_MS: i64 = 24 * 60 * 60 * SECOND_MS;

fn choice(id: &str, label: &str) -> Choice {
    Choice {
        id: id.to_owned(),
        label: label.to_owned(),
    }
}

/// The invocation an action question shows: a mail that would be sent.
fn draft() -> serde_json::Value {
    json!({"draft": {
        "action_label": "Send message",
        "resource_label": "Re: Are you raising?",
        "fields": [
            {"path": ["to"], "label": "To",
                "value": {"type": "text", "text": "jonas@northzone.example"}},
            {"path": ["body"], "label": "Body",
                "value": {"type": "text", "text": "We are not raising now."}},
        ],
    }})
}

fn ask_action() -> chat_messages::Ask {
    chat_messages::Ask {
        channel: Id::from_raw("c_general"),
        text: NonBlank::parse("Jonas asked whether you are raising. Send this answer?")
            .expect("words"),
        answers: Some(vec![
            choice(ALLOW, ""),
            choice(ALWAYS, ""),
            choice(DENY, ""),
        ]),
        kind: Some(ACTION.to_owned()),
        detail: Some(draft()),
        parent: None,
    }
}

fn answer(message: &Id, answer: &str, device: &str) -> chat_messages::AnswerQuestion {
    chat_messages::AnswerQuestion {
        message: message.clone(),
        answer: answer.to_owned(),
        device: Some(device.to_owned()),
        detail: None,
    }
}

/// One part of a chat's messages, written by whoever a test says.
struct Part {
    doc: AutoCommit,
    chat: crate::chat::Chat,
}

/// Who a verb is attributed to, as a host admits them.
#[derive(Clone, Copy)]
enum By {
    /// The person, themselves, at a device of their own.
    Person(&'static str),
    /// The agent [`AGENT`] acting for the person.
    AgentFor(&'static str),
    /// A bot installed by the person.
    BotOf(&'static str),
}

impl Part {
    fn new() -> Self {
        Self {
            doc: AutoCommit::new(),
            chat: crate::demo::demo(),
        }
    }

    fn apply(
        &mut self,
        by: By,
        now_ms: i64,
        invoke: impl Into<ui::tilo_resource::Invoke<ChatMessages>>,
    ) -> Result<(), ui::tilo_actions::ApiError> {
        let attribute = move |replica: Replica<ChatMessages>| match by {
            By::Person(person) => replica.as_invoker(person),
            By::AgentFor(person) => replica.as_agent_for(AGENT, person),
            By::BotOf(person) => replica.as_bot_for("b_bridge", person),
        };
        apply_part_doc_as(&mut self.doc, &self.chat, attribute, Some(now_ms), invoke)
    }

    /// The one message that asks something, and its question.
    fn asked(&self) -> (Id, Question) {
        let part = ChatMessages::read(&self.doc).expect("the part reads");
        part.messages
            .iter()
            .find_map(|(id, message)| Some((id.clone(), message.question.clone()?)))
            .expect("a message asks")
    }

    /// The agent asks its person about the mail, at `now_ms`.
    fn asking(now_ms: i64) -> (Self, Id) {
        let mut part = Self::new();
        part.apply(
            By::AgentFor(ME),
            now_ms,
            ChatMessages::ASK.with(ask_action()),
        )
        .expect("an agent asks the person it acts for");
        let (id, _) = part.asked();
        (part, id)
    }
}

#[test]
fn a_question_is_put_to_the_person_its_asker_acts_for_and_to_nobody_an_argument_names() {
    let (part, _) = Part::asking(0);
    let (_, question) = part.asked();
    assert_eq!(question.person, MemberId::person(ME));
    assert_eq!(question.kind.0, ACTION);
    assert!(question.waiting());
    assert_eq!(
        question.draft().expect("it keeps its draft").action_label,
        "Send message"
    );

    // A person asks in their own words, and a bot has no person to ask.
    let mut part = Part::new();
    assert!(
        part.apply(By::Person(ME), 0, ChatMessages::ASK.with(ask_action()))
            .is_err(),
        "a person acts for nobody"
    );
    assert!(
        part.apply(By::BotOf(ME), 0, ChatMessages::ASK.with(ask_action()))
            .is_ok(),
        "a bot asks the person who installed it"
    );
    assert_eq!(part.asked().1.person, MemberId::person(ME));

    // What it offers is checked where it is asked.
    let refused = |change: fn(&mut chat_messages::Ask)| {
        let mut ask = ask_action();
        change(&mut ask);
        Part::new()
            .apply(By::AgentFor(ME), 0, ChatMessages::ASK.with(ask))
            .is_err()
    };
    assert!(refused(|ask| {
        ask.kind = None;
        ask.answers = None;
    }));
    assert!(
        refused(|ask| {
            ask.kind = None;
        }),
        "a choice words every answer"
    );
    assert!(refused(|ask| {
        ask.answers = Some(vec![choice(ALLOW, ""), choice(ALLOW, "")]);
    }));
    assert!(refused(|ask| {
        ask.answers = Some((0..7).map(|n| choice(&format!("a{n}"), "A")).collect());
    }));
    assert!(refused(|ask| ask.kind = Some("Not A Word".to_owned())));
    assert!(refused(|ask| {
        ask.detail = Some(json!("x".repeat(crate::question::MAX_DETAIL_BYTES)));
    }));
}

#[test]
fn only_the_person_asked_answers_themselves_and_the_question_waits_for_them() {
    let (mut part, id) = Part::asking(0);

    // Days later it still waits: nothing about a question expires.
    let later = 3 * DAY_MS;
    for (who, by) in [
        ("the agent that asked, acting for them", By::AgentFor(ME)),
        ("a bot of theirs", By::BotOf(ME)),
        ("another member", By::Person("u_aria")),
    ] {
        assert!(
            part.apply(
                by,
                later,
                ChatMessages::ANSWER_QUESTION.with(answer(&id, ALLOW, "Laptop"))
            )
            .is_err(),
            "{who} does not answer"
        );
        assert!(part.asked().1.waiting(), "it still waits after {who} tried");
    }
    assert!(
        part.apply(
            By::Person(ME),
            later,
            ChatMessages::ANSWER_QUESTION.with(answer(&id, "maybe", "Laptop"))
        )
        .is_err(),
        "an answer it does not offer is no answer"
    );

    part.apply(
        By::Person(ME),
        later,
        ChatMessages::ANSWER_QUESTION.with(answer(&id, ALLOW, "Phone")),
    )
    .expect("its person answers it");
    let (_, question) = part.asked();
    let QuestionState::Answered(given) = question.state() else {
        panic!("it is answered: {:?}", question.state());
    };
    assert_eq!(given.answer.0, ALLOW);
    assert_eq!(given.device.0, "Phone", "it says where it was pressed");
    assert_eq!(given.at, later, "and when");

    // The same answer pressed on a second device changes nothing; another
    // answer is refused, and so is taking the question back.
    part.apply(
        By::Person(ME),
        later + SECOND_MS,
        ChatMessages::ANSWER_QUESTION.with(answer(&id, ALLOW, "Laptop")),
    )
    .expect("the same answer again is no failure");
    assert_eq!(
        part.asked().1.answered.expect("answered").device.0,
        "Phone",
        "the first press stands"
    );
    assert!(
        part.apply(
            By::Person(ME),
            later + SECOND_MS,
            ChatMessages::ANSWER_QUESTION.with(answer(&id, DENY, "Laptop"))
        )
        .is_err()
    );
    assert!(
        part.apply(
            By::AgentFor(ME),
            later + SECOND_MS,
            ChatMessages::WITHDRAW_QUESTION.with(chat_messages::WithdrawQuestion {
                message: id,
                why: None,
            })
        )
        .is_err(),
        "an answered question is not taken back"
    );
}

#[test]
fn its_asker_says_what_came_of_the_answer_once() {
    let (mut part, id) = Part::asking(0);
    let settle = |outcome: &str| {
        ChatMessages::SETTLE_QUESTION.with(chat_messages::SettleQuestion {
            message: id.clone(),
            outcome: outcome.to_owned(),
            detail: None,
        })
    };
    assert!(
        part.apply(By::AgentFor(ME), SECOND_MS, settle(DONE))
            .is_err(),
        "nothing came of an answer nobody gave"
    );
    part.apply(
        By::Person(ME),
        SECOND_MS,
        ChatMessages::ANSWER_QUESTION.with(answer(&id, ALLOW, "Phone")),
    )
    .unwrap();
    assert!(
        part.apply(By::Person(ME), 2 * SECOND_MS, settle(DONE))
            .is_err(),
        "only its asker says what came of it"
    );
    assert!(
        part.apply(By::AgentFor(ME), 2 * SECOND_MS, settle("sent"))
            .is_err(),
        "an outcome is started, done, dropped or failed"
    );
    // It says it started before it carries the answer out, so whatever
    // interrupts it, the answer is never carried out twice.
    part.apply(By::AgentFor(ME), 2 * SECOND_MS, settle(STARTED))
        .expect("its asker says it started");
    assert!(
        part.apply(By::AgentFor(ME), 2 * SECOND_MS, settle(DROPPED))
            .is_err(),
        "what was started ends done or failed"
    );
    part.apply(By::AgentFor(ME), 2 * SECOND_MS, settle(DONE))
        .expect("its asker says it is done");
    let (_, question) = part.asked();
    assert!(matches!(
        question.state(),
        QuestionState::Settled(given, outcome) if given.answer.0 == ALLOW && outcome.outcome.0 == DONE
    ));
    part.apply(By::AgentFor(ME), 3 * SECOND_MS, settle(DONE))
        .expect("saying it again changes nothing");
    assert!(
        part.apply(
            By::AgentFor(ME),
            3 * SECOND_MS,
            settle(crate::question::FAILED)
        )
        .is_err(),
        "a question settles once"
    );
}

#[test]
fn a_question_taken_back_stops_waiting_and_keeps_what_was_asked() {
    let (mut part, id) = Part::asking(0);
    let withdraw = |why: Option<&str>| {
        ChatMessages::WITHDRAW_QUESTION.with(chat_messages::WithdrawQuestion {
            message: id.clone(),
            why: why.map(str::to_owned),
        })
    };
    assert!(
        part.apply(By::Person("u_aria"), SECOND_MS, withdraw(None))
            .is_err(),
        "only its asker takes it back"
    );
    assert!(
        part.apply(By::Person(ME), SECOND_MS, withdraw(None))
            .is_err(),
        "its person answers it instead"
    );
    part.apply(
        By::AgentFor(ME),
        SECOND_MS,
        withdraw(Some("The mail was answered elsewhere")),
    )
    .expect("its asker takes it back");
    let (_, question) = part.asked();
    let QuestionState::Withdrawn(withdrawn) = question.state() else {
        panic!("it is taken back: {:?}", question.state());
    };
    assert_eq!(withdrawn.why, "The mail was answered elsewhere");
    assert_eq!(
        question.draft().expect("the draft stays").action_label,
        "Send message"
    );
    assert!(
        part.apply(
            By::Person(ME),
            2 * SECOND_MS,
            ChatMessages::ANSWER_QUESTION.with(answer(&id, ALLOW, "Phone"))
        )
        .is_err(),
        "a question taken back takes no answer"
    );
    part.apply(By::AgentFor(ME), 2 * SECOND_MS, withdraw(None))
        .expect("taking it back again changes nothing");
}

// --- Chat's window ---------------------------------------------------------

const CHANNEL: &str = "c_chat";

/// The person at the window, as what they post is attributed.
fn window_person(app: &mut TestApp<ChatShell>) -> MemberId {
    crate::testing::mounted_post(app, CHANNEL, "Good morning");
    app.settle();
    mounted_rows(app, CHANNEL)
        .last()
        .expect("the post is shown")
        .author
        .clone()
}

/// Press the button of `answer` under the message `message`, where it is drawn.
fn press(app: &mut TestApp<ChatShell>, message: &str, answer: &str) {
    app.settle();
    app.frame();
    let wanted = format!("chat.answer:{message}:{answer}");
    let rect = app
        .chat()
        .hits
        .iter()
        .find(|(_, hit)| hit.semantic_id() == wanted)
        .map(|(rect, _)| *rect)
        .expect("the answer is offered as a button");
    app.harness().click_at(rect.center().x, rect.center().y);
    app.settle();
}

/// The agent acting for `person`, as a host admits its calls.
fn agent_for(person: &MemberId) -> ActionInvoker {
    ActionInvoker::admitted(&Acting::agent(AGENT).acting_for(Acting::person(person.id())))
}

fn asked_in(app: &mut TestApp<ChatShell>) -> crate::model::Message {
    app.settle();
    mounted_rows(app, CHANNEL)
        .into_iter()
        .rfind(|m| m.question.is_some())
        .expect("the question is posted")
}

fn row(app: &mut TestApp<ChatShell>, id: &str) -> crate::model::Message {
    app.settle();
    mounted_rows(app, CHANNEL)
        .into_iter()
        .find(|m| m.id == id)
        .expect("the message is shown")
}

fn answer_buttons(app: &mut TestApp<ChatShell>) -> Vec<String> {
    app.settle();
    app.frame();
    app.chat()
        .hits
        .iter()
        .map(|(_, hit)| hit.semantic_id())
        .filter(|id| id.starts_with("chat.answer:"))
        .collect()
}

fn ask_action_in(app: &mut TestApp<ChatShell>, person: &MemberId) -> crate::model::Message {
    app.call_by(
        agent_for(person),
        &chain(),
        "ask",
        json!({
            "channel": CHANNEL,
            "text": "Jonas asked whether you are raising. Send this answer?",
            "kind": ACTION,
            "answers": [{"id": ALLOW, "label": ""}, {"id": ALWAYS, "label": ""},
                {"id": DENY, "label": ""}],
            "detail": draft(),
        }),
    )
    .expect("the agent asks its person");
    asked_in(app)
}

#[test]
fn the_window_draws_an_action_question_as_a_card_with_named_buttons_and_takes_the_press() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let me = window_person(&mut app);
    let asked = ask_action_in(&mut app, &me);

    // What would happen is read aloud with the message.
    let label = app.chat().model_message_label(&asked);
    for said in [
        "Send message",
        "On “Re: Are you raising?”",
        "To: jonas@northzone.example",
        "Body: We are not raising now.",
        "It can’t be undone.",
    ] {
        assert!(label.contains(said), "the card says {said:?}: {label}");
    }

    // Each answer is a button a screen reader names by what it does.
    assert_eq!(
        answer_buttons(&mut app),
        [ALLOW, ALWAYS, DENY].map(|answer| format!("chat.answer:{}:{answer}", asked.id))
    );
    let named: Vec<(String, ui::a11y::Role)> = {
        let chat = app.chat();
        chat.semantic_hits()
            .into_iter()
            .filter(|(_, hit, ..)| matches!(hit, crate::app::Hit::Answer { .. }))
            .map(|(_, _, name, role, _)| (name.text, role))
            .collect()
    };
    assert_eq!(
        named,
        ["Allow once", "Always allow for this event", "Don’t allow"]
            .map(|name| (name.to_owned(), ui::a11y::Role::Button))
    );

    // Pressing one is the person's answer; the buttons leave with the wait.
    press(&mut app, &asked.id, ALLOW);
    let answered = row(&mut app, &asked.id);
    let question = answered.question.clone().expect("it still asks");
    assert!(
        matches!(question.state(), QuestionState::Answered(given) if given.answer.0 == ALLOW),
        "{:?}",
        question.state()
    );
    assert!(answer_buttons(&mut app).is_empty());
    let label = app.chat().model_message_label(&answered);
    assert!(
        label.ends_with("You answered: allowed once"),
        "it says how it stands: {label}"
    );
}

#[test]
fn a_question_for_someone_else_shows_that_it_waits_and_offers_this_reader_nothing() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let asked = ask_action_in(&mut app, &MemberId::person("u_bob"));
    assert!(answer_buttons(&mut app).is_empty());
    let shown = app.chat().question_shown(&asked).expect("it shows");
    assert!(shown.answers.is_empty());
    assert!(
        shown.spoken().contains("Waiting for"),
        "it says whom it waits for: {}",
        shown.spoken()
    );
    assert!(
        app.call_on(
            &message_item(&asked.id),
            "answer-question",
            json!({"answer": ALLOW}),
        )
        .is_err(),
        "and the resource refuses this reader's answer"
    );
}

#[test]
fn a_choice_posts_the_answer_pressed_as_the_persons_own_message() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let me = window_person(&mut app);
    app.call_by(
        agent_for(&me),
        &chain(),
        "ask",
        json!({
            "channel": CHANNEL,
            "text": "When should I send the summary?",
            "answers": [{"id": "today", "label": "Today at 18:00"},
                {"id": "tomorrow", "label": "Tomorrow morning"}],
        }),
    )
    .expect("the agent asks");
    let asked = asked_in(&mut app);
    assert_eq!(answer_buttons(&mut app).len(), 2);

    press(&mut app, &asked.id, "tomorrow");
    let rows = mounted_rows(&mut app, CHANNEL);
    let said = rows.last().expect("a message follows");
    assert_eq!(said.text, "Tomorrow morning");
    assert_eq!(said.author, me, "it is the person's own message");
    assert!(said.question.is_none());
    let question = row(&mut app, &asked.id).question.expect("it asked");
    assert!(
        matches!(question.state(), QuestionState::Answered(given) if given.answer.0 == "tomorrow")
    );
    assert!(answer_buttons(&mut app).is_empty());
}

#[test]
fn words_typed_instead_close_a_choice_and_leave_an_action_question_waiting() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let me = window_person(&mut app);
    app.call_by(
        agent_for(&me),
        &chain(),
        "ask",
        json!({
            "channel": CHANNEL,
            "text": "Today or tomorrow?",
            "answers": [{"id": "today", "label": "Today"}, {"id": "tomorrow", "label": "Tomorrow"}],
        }),
    )
    .expect("the agent asks");
    let action = ask_action_in(&mut app, &me);
    assert_eq!(answer_buttons(&mut app).len(), 5);
    crate::testing::mounted_post(&mut app, CHANNEL, "Neither, on Friday");
    assert_eq!(
        answer_buttons(&mut app),
        [ALLOW, ALWAYS, DENY].map(|answer| format!("chat.answer:{}:{answer}", action.id)),
        "the choice is answered in words; the action still waits for its answer"
    );
}

#[test]
fn a_question_survives_the_window_closing_and_is_answered_after() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let me = window_person(&mut app);
    let asked = ask_action_in(&mut app, &me);
    let mut app = app.reopen().expect("the app opens again");
    assert!(
        row(&mut app, &asked.id)
            .question
            .expect("it still asks")
            .waiting()
    );
    assert_eq!(answer_buttons(&mut app).len(), 3);
    press(&mut app, &asked.id, DENY);
    let question = row(&mut app, &asked.id).question.expect("it asked");
    assert!(matches!(question.state(), QuestionState::Answered(given) if given.answer.0 == DENY));
}
