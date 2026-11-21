//! What an agent asks its person in its own words through the API server
//! (`tilo_ask`): the question waits where every question an agent puts to its
//! person waits, on the person's own devices, and the agent's call answers
//! with what they said there: one of its choices, or that they chose not to
//! answer. A question offers choices; words come as a reply in the conversation.
use super::api::{ISSUER, Mcp};
use super::api_on_server::api_on_server;
use super::api_questions::agent_with_a_book;
use super::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tilo_server_client::MemberApprover;
use tilo_server_client::questions::{Answered, MemberQuestions, QuestionSheet, answer_questions};
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tokio::sync::mpsc;

/// The agent session the agent asks in, by the title it gives it.
const TITLE: &str = "Plan Jane Doe’s visa appointment";

/// The person at their page, answering each question with what `answer`
/// makes of it, and noting what they were asked.
struct Person {
    answer: fn(&PendingConfirmation) -> ConfirmationAnswer,
    answered: mpsc::UnboundedSender<Answered>,
    asked: Arc<Mutex<Vec<PendingConfirmation>>>,
}

impl QuestionSheet for Person {
    fn ask(&mut self, question: &PendingConfirmation) {
        self.asked.lock().unwrap().push(question.clone());
        let _ = self.answered.send((question.id, (self.answer)(question)));
    }

    fn withdraw(&mut self, _id: [u8; 32]) {}
}

/// `tilo_ask` with `arguments`, while the page holding `root` answers what
/// the server asks with `answer`: the call's reply and what the page was asked.
async fn asked(
    mcp: &mut Mcp<'_>,
    approver: &MemberApprover,
    root: &SigningKey,
    arguments: Value,
    answer: fn(&PendingConfirmation) -> ConfirmationAnswer,
) -> (Value, Vec<PendingConfirmation>) {
    let mut page = MemberQuestions {
        server: approver,
        root,
        device: None,
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (answered, mut answers) = mpsc::unbounded_channel();
    let mut person = Person {
        answer,
        answered,
        asked: Arc::clone(&seen),
    };
    let session = answer_questions(&mut page, &mut person, &mut answers, || {
        tokio::time::sleep(Duration::from_millis(50))
    });
    tokio::pin!(session);
    let call = mcp.post(json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                               "params": {"name": "tilo_ask", "arguments": arguments,
                                          "_meta": {"so.tilo.mcp": {"invocation": {
                                              "id": "visa-2", "title": TITLE}}}}}));
    let (status, reply) = tokio::select! {
        reply = call => reply,
        error = &mut session => panic!("the page's session with the server ended: {error}"),
    };
    assert_eq!(status, 200, "{reply}");
    let seen = seen.lock().unwrap().clone();
    (reply, seen)
}

/// The text the call answered with.
fn said(reply: &Value) -> String {
    assert_eq!(
        reply["result"]["isError"], false,
        "the call answers: {reply}"
    );
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_person_answers_what_the_agent_asks_and_the_agent_hears_it() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let mut agent = agent_with_a_book(&on_server, &ada).await;
    let approver = MemberApprover::new(on_server.server_host());

    // A decision between two choices: the person picks one.
    let (reply, seen) = Box::pin(asked(
        &mut agent,
        &approver,
        &ada,
        json!({"question": "Which consulate should I book?",
               "choices": ["Berlin", "Munich"]}),
        |_| ConfirmationAnswer::Reply("Munich".into()),
    ))
    .await;
    assert_eq!(said(&reply), "answered: Munich");
    assert_eq!(seen.len(), 1, "asked once: {seen:?}");
    // Who asks, in which agent session, for whom and where, as the system names them;
    // what is asked, in the agent's words.
    let question = &seen[0];
    assert_eq!(question.agent_label, "Jane Doe’s agent");
    assert_eq!(question.title, TITLE);
    assert_eq!(question.workspace, "Applicant: Jane Doe");
    let ask = question.ask.as_ref().expect("an agent's own question");
    assert_eq!(ask.question, "Which consulate should I book?");
    assert_eq!(ask.choices, ["Berlin", "Munich"]);
    assert!(
        question.action_label.is_empty(),
        "it asks to allow no action"
    );

    // Not now: the agent hears that the person chose not to answer.
    let (reply, _) = Box::pin(asked(
        &mut agent,
        &approver,
        &ada,
        json!({"question": "Book the earliest appointment?", "choices": ["Yes", "No"]}),
        |_| ConfirmationAnswer::Deny,
    ))
    .await;
    assert!(
        said(&reply).starts_with("declined"),
        "a person who does not answer is not answered for: {reply}"
    );

    approver.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_that_is_no_question_is_refused_before_anyone_is_asked() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let mut agent = agent_with_a_book(&on_server, &ada).await;
    let (status, reply) = agent
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "tilo_ask",
                                "arguments": {"question": "Which?", "choices": ["A", "A"]}}}))
        .await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(reply.to_string().contains("each once"), "{reply}");

    // A question without choices could be answered only in the person's own words: it is
    // refused, and the agent is told to ask in the conversation and read the reply there.
    let (status, reply) = agent
        .post(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                     "params": {"name": "tilo_ask",
                                "arguments": {"question": "Which dates can Jane travel?"}}}))
        .await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(
        reply
            .to_string()
            .contains("ask in the conversation and read their reply there"),
        "{reply}"
    );
}
