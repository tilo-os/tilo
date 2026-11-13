//! Subscription sign-ins through the production Rig codec: a `ChatGPT`
//! sign-in speaks Responses to the `ChatGPT` backend for its account, and a
//! Copilot sign-in names the editor GitHub's API requires.
use super::*;
use base64::Engine as _;
use serde_json::json;
use std::fmt::Write as _;
use test_http::{Reply, Responder};

/// A `ChatGPT` access token: a JWT naming `account` in `OpenAI`'s claim.
fn chatgpt_token(account: &str) -> String {
    let encode = |value: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value);
    format!(
        "{}.{}.signature",
        encode(br#"{"alg":"RS256"}"#),
        encode(
            json!({"https://api.openai.com/auth": {"chatgpt_account_id": account}})
                .to_string()
                .as_bytes()
        )
    )
}

/// The event stream the `ChatGPT` backend answers every request with.
fn responses_stream(text: &str) -> String {
    let message = json!({
        "type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
        "content": [{"type": "output_text", "annotations": [], "text": text}]
    });
    let response = |status: &str, output: serde_json::Value| {
        json!({
            "id": "resp_1", "object": "response", "created_at": 0, "status": status,
            "model": "gpt-5-codex", "output": output, "tools": []
        })
    };
    let mut added = message.clone();
    added["content"] = json!([]);
    added["status"] = json!("in_progress");
    let events = [
        json!({"type": "response.created", "sequence_number": 0, "response": response("in_progress", json!([]))}),
        json!({"type": "response.output_item.added", "output_index": 0, "sequence_number": 1, "item": added}),
        json!({"type": "response.output_text.delta", "item_id": "msg_1", "output_index": 0, "content_index": 0, "sequence_number": 2, "delta": text}),
        json!({"type": "response.output_item.done", "output_index": 0, "sequence_number": 3, "item": message}),
        json!({"type": "response.completed", "sequence_number": 4, "response": response("completed", json!([message]))}),
    ];
    let mut stream = String::new();
    for event in &events {
        writeln!(
            stream,
            "event: {}\ndata: {event}\n",
            event["type"].as_str().unwrap()
        )
        .unwrap();
    }
    stream
}

#[tokio::test(flavor = "current_thread")]
async fn a_chatgpt_sign_in_asks_its_accounts_backend_in_responses() {
    let stream = responses_stream("hello from the subscription");
    let served = Responder::start(vec![
        Reply::status(200)
            .header("Content-Type", "text/event-stream")
            .body(stream),
    ]);
    let mut profile = agent_provider_config::presets::Preset::CHATGPT.profile();
    profile.endpoint = format!("{}/backend-api/codex", served.url());
    profile.models = vec![agent_provider_config::ModelDescriptor::new("gpt-5-codex")];
    let token = chatgpt_token("acct-7");
    let request =
        neutral_request(neutral::Conversation::new("instructions", "question").request(vec![], 64))
            .unwrap();
    let response = host_complete(
        &provider_client().unwrap(),
        &profile,
        Some(&token),
        "gpt-5-codex",
        request,
        tilo_test_env::hang_bound(),
        crate::openai::Target::Standard,
    )
    .await
    .map_err(|failure| failure.why)
    .unwrap();
    let response = neutral_response(response).unwrap();
    assert!(
        serde_json::to_string(&response)
            .unwrap()
            .contains("hello from the subscription")
    );

    let seen = served.next_request();
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.target, "/backend-api/codex/responses");
    assert_eq!(seen.header("chatgpt-account-id"), Some("acct-7"));
    assert_eq!(seen.header("originator"), Some("tilo"));
    assert!(
        seen.header("authorization")
            .is_some_and(|value| value.starts_with("Bearer ey")),
        "{:?}",
        seen.header("authorization")
    );
    let body: serde_json::Value = serde_json::from_str(&seen.body_text()).unwrap();
    // The backend streams every answer, keeps none, and takes no
    // output limit or temperature.
    assert_eq!(body["model"], "gpt-5-codex");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    for refused in ["max_output_tokens", "max_tokens", "temperature"] {
        assert!(body.get(refused).is_none(), "{refused}");
    }
    assert!(
        body["instructions"]
            .as_str()
            .is_some_and(|text| text.contains("instructions"))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_copilot_sign_in_names_the_editor_and_speaks_chat() {
    let reply = json!({"id":"r","object":"chat.completion","created":0,"model":"gpt-4.1","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hi"}}]});
    let served = Responder::start(vec![
        Reply::status(200)
            .header("Content-Type", "application/json")
            .body(reply.to_string()),
    ]);
    let mut profile = agent_provider_config::presets::Preset::COPILOT.profile();
    profile.endpoint = served.url();
    let request =
        neutral_request(neutral::Conversation::new("instructions", "question").request(vec![], 64))
            .unwrap();
    host_complete(
        &provider_client().unwrap(),
        &profile,
        Some("tid=copilot-token"),
        "gpt-4.1",
        request,
        tilo_test_env::hang_bound(),
        crate::openai::Target::Standard,
    )
    .await
    .map_err(|failure| failure.why)
    .unwrap();

    let seen = served.next_request();
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.target, "/chat/completions");
    assert_eq!(
        seen.header("authorization"),
        Some("Bearer tid=copilot-token")
    );
    assert_eq!(seen.header("copilot-integration-id"), Some("vscode-chat"));
    assert!(seen.header("editor-version").is_some());
    let body: serde_json::Value = serde_json::from_str(&seen.body_text()).unwrap();
    assert_eq!(body["model"], "gpt-4.1");
}
