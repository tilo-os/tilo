//! A person's assistant on the server calls a service with a key it never sees, with no device
//! of theirs on while it works.
//!
//! The chain through the server's own interfaces, against a service the test starts on this
//! computer and a key that is made up: the person's device keeps the key in their vault; the
//! assistant's turn holds only the reference and makes the call; the server asks the person in
//! the assistant's reply and sends nothing; they allow it there; the assistant, asked again with
//! their answer, makes the same call and is answered what the service answered, the key struck
//! out of what the service repeated. The key is in no ask, no reply, no tool result, no model
//! request, no log line and nowhere on the server's disk in the clear.
use super::hosted_secret_use::{Hosted, does, files_holding, logged};
use test_http::{Reply as Answer, Responder};
use tilo_actions::remote::{person_telling::Kind, person_vault};
use tilo_server::workspaces::person_model::scripted::Reply;

/// A made-up key: no service ever issued it, and it opens nothing.
const KEY: &str = "made-up-key-for-tests-0123456789abcdef";
/// What stands in an answer where the key stood.
const WITHHELD: &str = "[withheld]";
/// What the agent is told when its person is asked first.
const ASKED: &str = "Your person is asked in your reply";

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
    let asked = hosted.questions().await;
    assert_eq!(
        asked,
        ["To read this month's numbers. Answer allow or no."],
        "one question about the call, why in the agent's words, and no always offered"
    );
    let told = hosted
        .workspaces
        .told_after(&hosted.root.verifying_key().to_bytes(), 0)
        .told;
    let question = told
        .iter()
        .find(|told| told.kind == Kind::Question)
        .expect("she is told the question");
    assert!(question.waits(), "the question waits for her");
    let id = reference.trim_start_matches("vault.item:");
    assert!(
        !question.words.contains(id) && !hosted.said().await.contains(id),
        "the item's address is withheld: {}",
        question.words
    );

    // They allow it, where they asked. The allow sends nothing by itself: the assistant is
    // asked again with their answer, and makes the same call.
    hosted.answers("allow").await;
    assert!(
        service.drain_requests().is_empty(),
        "an allow sends nothing by itself"
    );
    // The assistant makes the same call again: it is sent once, and the answer is its tool
    // result, with the key struck out of what the service repeated.
    let woken = hosted
        .woken_doing(vec![
            balance("call-2", &service, &reference),
            Reply::text("Your balance is 1234."),
        ])
        .await;
    assert!(
        woken.contains("They answered: allowed, this once") && woken.contains("make the same call"),
        "{woken}"
    );
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
        hosted.questions().await.len(),
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
    assert!(
        !hosted.said().await.contains(KEY),
        "an ask or a reply holds the key"
    );
    let saved = hosted.record_bytes().await;
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
