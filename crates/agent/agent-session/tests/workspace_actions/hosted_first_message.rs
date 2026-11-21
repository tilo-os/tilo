//! A person's first message, sent the moment their app says it is ready.
//!
//! In a person's first minute their app links a provider with the key they typed and then
//! chooses its model for the main thread with two asks, one after the other: first for what
//! answers there, then for what works there. It says it is ready after the first. A message
//! sent in that moment is a turn the server works as a task, the kind the second ask has not
//! named yet. The main thread is one choice, so the model chosen for its other kind answers:
//! asked with the person's own key, and nobody is told that no model is linked.
use tilo_actions::remote::person_models;

use super::*;

/// A made-up key: no provider ever issued it, and it opens nothing.
const KEY: &str = "made-up-key-of-a-first-minute-0123456789";
/// A made-up model of a made-up provider.
const MODEL: &str = "made-up-main";

/// One of the person's own model actions, asked from their device.
async fn asks(hosted: &mut Hosted, action: &str, argument: serde_json::Value) -> serde_json::Value {
    let asked = (person_models::PROVIDER, person_models::RESOURCE, action);
    hosted.on_device(asked, argument).await
}

/// What the assistant wrote to its person, oldest first.
async fn answered(hosted: &Hosted) -> Vec<String> {
    let part = conversation_part(&hosted.workspaces, &hosted.workspace).await;
    let mut theirs: Vec<_> = part
        .messages
        .iter()
        .filter(|(_, held)| held.author == MemberId::agent(SCOUT))
        .map(|(_, held)| held)
        .collect();
    theirs.sort_by_key(|held| held.ts);
    theirs.into_iter().map(|held| held.text.clone()).collect()
}

/// A person's first minute: nothing is linked. What this test's server linked for them before,
/// they unlink from their own device.
async fn unlinks_everything(hosted: &mut Hosted) {
    let before = asks(hosted, person_models::LIST, serde_json::Value::Null).await;
    for linked in before["providers"].as_array().expect("their providers") {
        let unlinking = serde_json::json!({"provider": linked["id"]});
        asks(hosted, person_models::REMOVE_PROVIDER, unlinking).await;
    }
    let nothing = asks(hosted, person_models::LIST, serde_json::Value::Null).await;
    assert_eq!(
        nothing,
        serde_json::json!({"providers": []}),
        "nothing is linked"
    );
}

/// The person adds a provider in Settings, under Agent models, from their device: the provider,
/// the key they typed, a model of it. No test asks a real provider for its list of models, so
/// the provider is added by its address and its one model is named by hand. The provider's id.
async fn links_a_provider(hosted: &mut Hosted) -> String {
    let adding = serde_json::json!({
        "name": "Made-up models",
        "protocol": "open-ai-compatible",
        "endpoint": "https://models.example.com/v1",
    });
    let added = asks(hosted, person_models::ADD_PROVIDER, adding).await;
    let provider = added["id"].as_str().expect("the provider's id").to_owned();
    let keeping = serde_json::json!({"provider": provider, "key": KEY});
    asks(hosted, person_models::SET_KEY, keeping).await;
    let naming = serde_json::json!({"provider": provider, "model": MODEL});
    asks(hosted, person_models::ADD_MODEL, naming).await;
    provider
}

/// The person's asks in their app's own order, a message between the two that choose the
/// model, and another after the second.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_first_message_between_the_two_asks_of_a_model_choice_is_answered_by_that_model() {
    let mut hosted = Hosted::start().await;
    unlinks_everything(&mut hosted).await;
    let provider = links_a_provider(&mut hosted).await;

    // The first of the two asks that choose the model: what answers in the main thread.
    let choosing =
        |kind: &str| serde_json::json!({"kinds": [kind], "provider": provider, "model": MODEL});
    let chosen = asks(
        &mut hosted,
        person_models::SET_ASSIGNMENTS,
        choosing("request"),
    )
    .await;
    // What the app reads as ready: a model for a request, at a provider that has its key.
    assert_eq!(
        chosen["assignments"],
        serde_json::json!([{"kind": "request", "provider": provider, "model": MODEL}]),
        "{chosen}"
    );
    assert_eq!(chosen["providers"][0]["configured"], true, "{chosen}");
    assert!(
        chosen.get("defaultProvider").is_none(),
        "the app's way chooses no default: {chosen}"
    );

    // The message of that moment.
    hosted
        .turn("Good morning", vec![Reply::text("Good morning, Ada.")])
        .await;
    assert_eq!(
        answered(&hosted).await,
        ["Good morning, Ada."],
        "the model they chose for the main thread answers"
    );
    let asked = hosted.model.asked();
    assert!(
        !asked.is_empty()
            && asked
                .iter()
                .all(|call| call.model == MODEL && call.key.as_deref() == Some(KEY)),
        "their own model is asked, with their own key: {asked:?}"
    );

    // The second ask arrives: what works in the main thread. The next message is answered as
    // the first was.
    let both = asks(
        &mut hosted,
        person_models::SET_ASSIGNMENTS,
        choosing("task"),
    )
    .await;
    assert_eq!(
        both["assignments"].as_array().map(Vec::len),
        Some(2),
        "{both}"
    );
    hosted
        .turn("And tomorrow?", vec![Reply::text("Tomorrow too.")])
        .await;
    assert_eq!(
        answered(&hosted).await,
        ["Good morning, Ada.", "Tomorrow too."]
    );
    hosted.page.peer.close();
}

/// A server whose operator gave its agents a model endpoint of its own (`TILO_AGENT_MODEL`, a
/// local model server here) answers with it while the person linked none. Once they link a
/// provider in Settings, under Agent models, their next message is answered by their own model,
/// with the key they typed, by the same running server: the endpoint is not asked again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_servers_own_endpoint_answers_until_the_person_links_a_model() {
    const ENDPOINT_SAYS: &str = "Good morning, from the server's own model.";
    let endpoint = test_http::Responder::handle(|_| {
        let completion = serde_json::json!({
            "id": "reply",
            "object": "chat.completion",
            "created": 1,
            "model": "local-model",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": ENDPOINT_SAYS},
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 7, "total_tokens": 12},
        });
        Some(
            test_http::Reply::status(200)
                .header("Content-Type", "application/json")
                .body(completion.to_string()),
        )
    });
    let mut hosted = Hosted::start().await;
    unlinks_everything(&mut hosted).await;
    // The server as its operator starts it: `TILO_AGENT_MODEL=openai:<endpoint>/v1`.
    let servers_own = tilo_agent_session::ModelChoice::parse(
        &format!("openai:{}/v1", endpoint.url()),
        Some("local-model"),
        tilo_agent_session::ApiKey::None,
    )
    .expect("the endpoint is a model");
    hosted.agents = Agents::for_test(
        hosted.workspaces.clone(),
        hosted.actions.clone(),
        Some(tilo_server::workspace_agents::model_of(servers_own)),
    );

    hosted.turn("Good morning", Vec::new()).await;
    assert_eq!(answered(&hosted).await, [ENDPOINT_SAYS]);
    let to_endpoint = endpoint.drain_requests();
    assert!(
        !to_endpoint.is_empty()
            && to_endpoint
                .iter()
                .all(|asked| asked.target == "/v1/chat/completions"),
        "the server's own endpoint answers: {:?}",
        to_endpoint
            .iter()
            .map(|asked| &asked.target)
            .collect::<Vec<_>>()
    );
    assert!(
        hosted.model.asked().is_empty(),
        "no model of the person's is asked while they linked none"
    );

    // They add their provider and choose its model for their main thread, as Settings does.
    let provider = links_a_provider(&mut hosted).await;
    let choosing = serde_json::json!({
        "kinds": ["request", "task"],
        "provider": provider,
        "model": MODEL,
    });
    asks(&mut hosted, person_models::SET_ASSIGNMENTS, choosing).await;

    hosted
        .turn("And now?", vec![Reply::text("Now from your own model.")])
        .await;
    assert_eq!(
        answered(&hosted).await,
        [ENDPOINT_SAYS, "Now from your own model."]
    );
    let asked = hosted.model.asked();
    assert!(
        !asked.is_empty()
            && asked
                .iter()
                .all(|call| call.model == MODEL && call.key.as_deref() == Some(KEY)),
        "their own model is asked, with their own key: {asked:?}"
    );
    assert!(
        endpoint.drain_requests().is_empty(),
        "the server's endpoint is not asked once the person linked theirs"
    );
    hosted.page.peer.close();
}
