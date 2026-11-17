//! A person's assistant on the server calls a service with a key it never sees, with no device
//! of theirs on while it works.
//!
//! The chain through the server's own interfaces, against a service the test starts on this
//! computer and a key that is made up: the person's device keeps the key in their vault; the
//! assistant's turn holds only the reference and makes the call; the server asks the person
//! and sends nothing; they allow it; the assistant makes the same call again and is answered
//! what the service answered, the key struck out of what the service repeated. The key is in
//! no message, no tool result, no model request, no log line and nowhere on the server's disk
//! in the clear.
use super::hosted_agent_turns::{SCOUT, conversation_part};
use super::hosted_secret_use::{Hosted, does, files_holding, logged};
use super::*;
use test_http::{Reply as Answer, Responder};
use tilo_actions::remote::person_vault;
use tilo_server::workspaces::person_model::scripted::Reply;

const CHAT: &str = "so.tilo.chat";
/// A made-up key: no service ever issued it, and it opens nothing.
const KEY: &str = "made-up-key-for-tests-0123456789abcdef";
/// What stands in an answer where the key stood.
const WITHHELD: &str = "[withheld]";
/// What the agent is told when its person is asked first.
const ASKED: &str = "Your person is asked in the conversation";

/// A service that keeps a number behind the made-up key. It is a careless one: it repeats
/// what it was shown in what it answers.
fn paying() -> Responder {
    Responder::handle(|request| {
        let shown = request.header("authorization").unwrap_or_default();
        if shown != format!("Bearer {KEY}") {
            return Some(Answer::status(401).body(r#"{"error":"that key is unknown"}"#));
        }
        Some(
            Answer::status(200)
                .header("Content-Type", "application/json")
                .body(format!(r#"{{"balance":1234,"shown":"{shown}"}}"#)),
        )
    })
}

/// The call for the balance at `service` with the vault item `reference` names.
fn balance(call: &str, service: &Responder, reference: &str) -> Reply {
    does(
        call,
        "vault/vault#web-call",
        &serde_json::json!({
            "url": format!("{}/v1/balance", service.url()),
            "key": reference,
            "reason": "To read this month's numbers.",
        }),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_assistant_calls_a_service_with_a_key_it_never_sees() {
    let lines = logged();
    let mut hosted = Hosted::start().await;
    // The test's service is on this computer, where a server calls only if its operator lets it.
    hosted.workspaces.call_private_model_addresses(true);
    let service = paying();

    // The protected path: the person's device hands the key to their vault on the server.
    let kept = hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD,
            ),
            serde_json::json!({"kind": "key", "belongsTo": [service.url()], "secret": KEY}),
        )
        .await;
    let reference = kept["reference"].as_str().expect("a reference").to_owned();

    // The turn holds the reference and makes the call. Nobody watches, so the server asks the
    // person and sends nothing.
    hosted
        .turn(
            "What is this month's balance?",
            vec![
                balance("call-1", &service, &reference),
                Reply::text("I asked you to allow the call. It waits for your answer."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(
        told.contains(ASKED),
        "the agent is told its person is asked: {told}"
    );
    assert!(
        service.drain_requests().is_empty(),
        "nothing is sent unasked"
    );
    let asked = hosted.questions(chat::question::ACTION).await;
    assert_eq!(asked.len(), 1, "one question about the call");
    let (question, held) = &asked[0];
    let asks = held.question.as_ref().unwrap();
    assert_eq!(held.author, MemberId::agent(SCOUT));
    assert_eq!(asks.person, MemberId::person(hosted.person()));
    assert_eq!(
        held.text, "To read this month's numbers.",
        "why, in the agent's words"
    );
    let offered: Vec<&str> = asks
        .answers
        .iter()
        .map(|answer| answer.id.as_str())
        .collect();
    assert_eq!(
        offered,
        [chat::question::ALLOW, chat::question::DENY],
        "no Always is offered for a call"
    );
    let item = asks
        .uses_item()
        .expect("the card says which item of the vault");
    assert_eq!(item.belongs_to, [service.url()]);
    let shown = format!("{:?} {}", asks.draft(), asks.detail.0);
    assert!(
        shown.contains("/v1/balance"),
        "where the call goes: {shown}"
    );
    let id = reference.trim_start_matches("vault.item:");
    assert!(
        !shown.contains(id),
        "the item's address is withheld: {shown}"
    );

    // They allow it. The server says how that ended, and still sends nothing by itself.
    hosted
        .answers(question, chat::question::ALLOW, serde_json::Value::Null)
        .await;
    hosted.agents.look(&hosted.workspace).await.unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let now = hosted.questions(chat::question::ACTION).await;
        let outcome = now[0].1.question.as_ref().unwrap().outcome.clone();
        if outcome
            .as_ref()
            .is_some_and(|outcome| outcome.outcome.0 == chat::question::DONE)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the server never took the answer: {outcome:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        service.drain_requests().is_empty(),
        "an allow sends nothing by itself"
    );

    // The assistant makes the same call again: it is sent once, and the answer is its tool
    // result, with the key struck out of what the service repeated.
    hosted
        .turn(
            "I allowed it.",
            vec![
                balance("call-2", &service, &reference),
                Reply::text("Your balance is 1234."),
            ],
        )
        .await;
    let told = hosted.last_tool_result();
    assert!(
        told.contains("1234") && !told.contains(ASKED),
        "the service's answer is the tool's result: {told}"
    );
    assert!(
        told.contains(&format!("Bearer {WITHHELD}")),
        "what the service repeated is struck: {told}"
    );
    assert!(!told.contains(KEY), "a tool result holds the key");
    let seen = service.drain_requests();
    assert_eq!(seen.len(), 1, "sent once");
    assert_eq!(seen[0].target, "/v1/balance");
    assert_eq!(
        seen[0].header("authorization"),
        Some(format!("Bearer {KEY}").as_str()),
        "the service is shown the key the vault keeps"
    );
    assert_eq!(
        hosted.questions(chat::question::ACTION).await.len(),
        1,
        "no second question for the call that was allowed"
    );

    // The key reached the service and nothing else.
    let requests = hosted.model.asked();
    assert!(
        requests.len() >= 4,
        "two turns of two calls each: {}",
        requests.len()
    );
    for call in &requests {
        assert!(
            !format!("{:?}", call.request).contains(KEY),
            "a model request holds the key"
        );
    }
    let last = format!("{:?}", requests.last().unwrap().request);
    assert!(
        last.contains("vault.item:"),
        "the model works with the reference"
    );
    let part = conversation_part(&hosted.workspaces, &hosted.workspace).await;
    let said = format!("{:?}", part.messages.iter().collect::<Vec<_>>());
    assert!(said.contains("/v1/balance") && !said.contains(KEY));
    let saved = hosted
        .workspaces
        .resource_document(&hosted.workspace, CHAT, chat::parts::part(0).as_str())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !saved
            .windows(KEY.len())
            .any(|window| window == KEY.as_bytes())
    );
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    assert!(
        logged.len() > 1_000,
        "the log was observed: {} bytes",
        logged.len()
    );
    assert!(!logged.contains(KEY), "a log line holds the key");
    let holding = files_holding(hosted.state.path(), KEY.as_bytes());
    assert!(
        holding.is_empty(),
        "in the clear on the server's disk: {holding:?}"
    );

    // What the item remembers of it: when, where and for whom.
    let remembers = hosted
        .on_device(
            (
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::USES,
            ),
            serde_json::json!({"item": reference}),
        )
        .await;
    assert_eq!(
        remembers["uses"].as_array().unwrap().len(),
        1,
        "{remembers}"
    );
    assert_eq!(remembers["uses"][0]["by"], "Scout");
    assert_eq!(
        remembers["uses"][0]["places"],
        serde_json::json!([service.url()])
    );
    assert!(!remembers.to_string().contains(KEY));
    hosted.page.peer.close();
}
