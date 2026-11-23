//! What an agent asks its person in its own words through the API server
//! (`tilo_ask`): the question is kept as its asked invocation record, listed with every other
//! question that waits for them, and the person answers it with the record's `resolve`: an
//! approve carries the choice they picked and the agent's call answers with it, and a decline
//! ends it declined. A question offers choices; words come as a reply in the conversation.
use super::api::{ISSUER, Mcp};
use super::api_on_server::api_on_server;
use super::api_questions::{agent_with_a_book, followed};
use super::*;
use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::asked::AskedBy;

/// The agent session the agent asks in, by the title it gives it.
const TITLE: &str = "Plan Jane Doe’s visa appointment";

/// `tilo_ask` with `arguments` as the agent: the invocation it waits in, and the reply that says
/// so.
async fn asking(agent: &mut Mcp<'_>, arguments: Value) -> (InvocationId, Value) {
    let (status, reply) = agent
        .post(json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                   "params": {"name": "tilo_ask", "arguments": arguments,
                              "_meta": {"so.tilo.mcp": {"structured": true, "invocation": {
                                  "id": "visa-2", "title": TITLE}}}}}))
        .await;
    assert_eq!(status, 200, "{reply}");
    let asked = &reply["result"]["structuredContent"];
    assert_eq!(
        asked["status"], "asked",
        "the question waits for its person: {reply}"
    );
    let invocation = asked["invocation_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the record it waits in: {reply}"))
        .parse()
        .expect("an invocation id");
    (invocation, reply)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_person_answers_what_the_agent_asks_and_the_agent_hears_it() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let mut with = agent_with_a_book(&on_server, &ada).await;

    // A decision between two choices: the person picks one at a device of theirs.
    let (invocation, _) = asking(
        &mut with.agent,
        json!({"question": "Which consulate should I book?",
               "choices": ["Berlin", "Munich"]}),
    )
    .await;
    // What is asked, in the agent's words, and each choice it offers; who asks, as the system
    // names them: never as the agent words itself.
    let asked = with.page.asked_about(invocation).await;
    let AskedBy::Agent(who) = &asked.facts.by else {
        panic!("an agent asks: {asked:?}");
    };
    assert_eq!(who.name, "Jane Doe’s agent", "{asked:?}");
    assert_eq!(
        asked.facts.words, "Which consulate should I book?",
        "{asked:?}"
    );
    assert_eq!(asked.facts.choices, ["Berlin", "Munich"], "{asked:?}");
    assert!(
        asked.name.is_empty(),
        "it asks to allow no action: {asked:?}"
    );
    with.page
        .resolve(&asked, "approve", Some(1))
        .await
        .expect("the person picks Munich");
    let answered = followed(&mut with.agent, invocation).await;
    assert_eq!(answered["status"], "ok", "{answered}");
    assert_eq!(
        answered["result"]["answered"]["choice"], "Munich",
        "{answered}"
    );

    // Not now: the agent hears that the person chose not to answer.
    let (invocation, _) = asking(
        &mut with.agent,
        json!({"question": "Book the earliest appointment?",
               "choices": ["Yes", "No"]}),
    )
    .await;
    let asked = with.page.asked_about(invocation).await;
    with.page
        .resolve(&asked, "decline", None)
        .await
        .expect("the person declines");
    let declined = followed(&mut with.agent, invocation).await;
    assert_eq!(declined["status"], "failed", "{declined}");
    assert_eq!(declined["error"]["code"], "cancelled", "{declined}");
    assert_eq!(declined["error"]["message"], "declined", "{declined}");
    with.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_that_is_no_question_is_refused_before_anyone_is_asked() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let mut with = agent_with_a_book(&on_server, &ada).await;
    let (status, reply) = with
        .agent
        .post(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                     "params": {"name": "tilo_ask",
                                "arguments": {"question": "Which?", "choices": ["A", "A"]}}}))
        .await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(reply.to_string().contains("each once"), "{reply}");

    // A question without choices could be answered only in the person's own words: it is
    // refused, and the agent is told to ask in the conversation and read the reply there.
    let (status, reply) = with
        .agent
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
    assert!(with.page.asks().await.is_empty(), "nobody was asked");
    with.page.peer.close();
}
