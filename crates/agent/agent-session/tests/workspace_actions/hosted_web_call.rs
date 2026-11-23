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
use super::hosted_secret_use::{Hosted, does, files_holding, logged, resumed};
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
const ASKED: &str = "asked:";

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
    let asked = hosted.page.asks().await;
    assert_eq!(asked.len(), 1, "one question about the call");
    let question = &asked[0];
    assert!(
        matches!(
            &question.facts.by,
            tilo_actions::remote::asked::AskedBy::Agent(who) if who.id == SCOUT
        ),
        "{question:?}"
    );
    assert_eq!(
        question.facts.words, "To read this month's numbers.",
        "why, in the agent's words"
    );
    assert!(
        question
            .facts
            .facts
            .contains(&tilo_actions::remote::asked::Fact::new("Item", "key")),
        "{question:?}"
    );
    assert!(
        question
            .facts
            .facts
            .iter()
            .any(|fact| fact.label == "Goes to" && fact.value.contains(&service.url())),
        "where the key would go: {question:?}"
    );
    assert_eq!(
        question.offers,
        [
            tilo_actions::remote::asked::Offer::Approve,
            tilo_actions::remote::asked::Offer::Decline
        ],
        "no Always is offered for a call"
    );
    let shown = format!("{question:?}");
    assert!(
        shown.contains("/v1/balance"),
        "where the call goes: {shown}"
    );
    let id = reference.trim_start_matches("vault.item:");
    assert!(
        !shown.contains(id),
        "the item's address is withheld: {shown}"
    );

    // Their approve re-admits the call, which is sent once: its answer is the resumed run's
    // result, with the key struck out of what the service repeated.
    hosted.model.then(Reply::text("Your balance is 1234."));
    let question = hosted.page.asks().await.remove(0);
    hosted
        .page
        .resolve(&question, "approve", None)
        .await
        .expect("the person allows it");
    let told = resumed(&mut hosted, "1234").await;
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
    assert!(
        hosted.page.asks().await.is_empty(),
        "no second question for the call that was allowed"
    );

    // The key reached the service and nothing else.
    let requests = hosted.model.asked();
    assert!(
        requests.len() >= 2,
        "the call and its answer: {}",
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
        remembers["uses"][0]["using"],
        format!("{}/{}", person_vault::PROVIDER, person_vault::WEB_CALL)
    );
    assert_eq!(remembers["uses"][0]["what"], "GET /v1/balance");
    assert_eq!(remembers["uses"][0]["allowed"], "once");
    assert_eq!(
        remembers["uses"][0]["places"],
        serde_json::json!([service.url()])
    );
    assert!(!remembers.to_string().contains(KEY));
    hosted.page.peer.close();
}
