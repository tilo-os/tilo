//! Every catalogue preset's request reaches a local server in its
//! protocol's shape: the path its endpoint names, the key in the one
//! header the preset says and in no other, and a body of that dialect
//! (chat completions, Responses or Anthropic messages) carrying the
//! model, the conversation and the tools.

use super::*;
use agent_provider_config::presets::{Authentication, Preset};
use agent_provider_config::{KeyHeader, OpenAiApi};
use serde_json::json;

/// What the server saw of one request.
struct Seen {
    path: String,
    headers: Vec<(String, String)>,
    body: serde_json::Value,
}

/// A server answering one request with what its path asks for.
fn server() -> (String, std::thread::JoinHandle<Seen>) {
    use std::io::{BufRead as _, Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let mut reader = std::io::BufReader::new(&mut socket);
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_owned();
        let (mut headers, mut length) = (Vec::new(), 0);
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            let (name, value) = line.trim_end().split_once(':').unwrap();
            let (name, value) = (name.to_ascii_lowercase(), value.trim().to_owned());
            if name == "content-length" {
                length = value.parse().unwrap();
            }
            headers.push((name, value));
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let answer = if path.ends_with("/v1/messages") {
            json!({"id": "msg_1", "type": "message", "role": "assistant", "model": "checked-model",
                "content": [{"type": "text", "text": "answer"}],
                "stop_reason": "end_turn", "stop_sequence": null,
                "usage": {"input_tokens": 1, "output_tokens": 1}})
        } else if path.ends_with("/responses") {
            json!({"id": "resp_1", "object": "response", "created_at": 1, "model": "checked-model",
                "status": "completed", "incomplete_details": null,
                "output": [{"type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                    "content": [{"type": "output_text", "text": "answer", "annotations": []}]}],
                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}})
        } else {
            json!({"id": "reply", "object": "chat.completion", "created": 1, "model": "checked-model",
                "choices": [{"index": 0, "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "answer"}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}})
        }
        .to_string();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len()).unwrap();
        Seen {
            path,
            headers,
            body: serde_json::from_slice(&body).unwrap(),
        }
    });
    (origin, task)
}

fn request() -> neutral::ModelRequest {
    let mut request = neutral::Conversation::new("instructions", "question").request(
        vec![neutral::ToolDefinition {
            name: "lookup".into(),
            description: "Look up a resource".into(),
            input_schema: json!({"type": "object"}),
        }],
        64,
    );
    request.tool_choice = neutral::ToolChoice::Auto;
    request
}

/// `endpoint`'s path, its account parts filled in.
fn endpoint_path(endpoint: &str) -> String {
    let mut endpoint = endpoint.to_owned();
    while let Some(part) = agent_provider_config::presets::unfilled_part(&endpoint) {
        endpoint = endpoint.replace(&part.to_owned(), "acme");
    }
    let url = url::Url::parse(&endpoint).unwrap();
    url.path().trim_end_matches('/').to_owned()
}

/// The dialect a preset's agent turns take.
#[derive(Debug, PartialEq, Eq)]
enum Dialect {
    Chat,
    Responses,
    Messages,
}

#[tokio::test(flavor = "current_thread")]
async fn every_preset_sends_its_protocols_request_with_its_key_header() {
    const KEY: &str = "catalogue-test-key";
    let mut dialects = std::collections::BTreeSet::new();
    for preset in Preset::ALL.iter().copied().filter(|preset| {
        // A sign-in's request is the subscription backend's, proven by
        // the subscription tests with a minted token.
        !preset.signs_in_only()
    }) {
        let (origin, peer) = server();
        let mut profile = preset.profile();
        let path = endpoint_path(preset.endpoint());
        profile.endpoint = format!("{origin}{path}");
        let secret = match preset.authentication() {
            Authentication::None => None,
            _ => Some(KEY),
        };
        let response = host_complete(
            &provider_client().unwrap(),
            &profile,
            secret,
            "checked-model",
            neutral_request(request()).unwrap(),
            tilo_test_env::hang_bound(),
            crate::openai::Target::Standard,
        )
        .await;
        let seen = peer.join().unwrap();
        let response =
            response.unwrap_or_else(|failure| panic!("{}: {}", preset.slug(), failure.why));
        let answer = neutral_response(response).unwrap();
        assert!(
            answer.message.content.iter().any(|content| matches!(
                content, neutral::Content::Text { text } if text == "answer"
            )),
            "{}: {answer:?}",
            preset.slug()
        );

        let dialect = match (
            preset.protocol(),
            agent_provider_config::transform::openai_api(&profile),
        ) {
            (ProviderProtocol::Anthropic, _) => Dialect::Messages,
            (_, Some(OpenAiApi::Responses)) => Dialect::Responses,
            _ => Dialect::Chat,
        };
        let expected_path = match dialect {
            Dialect::Chat => format!("{path}/chat/completions"),
            Dialect::Responses => format!("{path}/responses"),
            Dialect::Messages => format!("{path}/v1/messages"),
        };
        assert_eq!(seen.path, expected_path, "{}", preset.slug());

        // The key travels in exactly the header the preset names.
        let credentials: Vec<_> = seen
            .headers
            .iter()
            .filter(|(name, _)| matches!(name.as_str(), "authorization" | "x-api-key" | "api-key"))
            .collect();
        match secret {
            None => assert!(credentials.is_empty(), "{}: {credentials:?}", preset.slug()),
            Some(key) => {
                let value = match preset.key_header() {
                    KeyHeader::Bearer => format!("Bearer {key}"),
                    KeyHeader::XApiKey | KeyHeader::ApiKey => key.to_owned(),
                };
                assert_eq!(
                    credentials,
                    [&(preset.key_header().name().to_owned(), value)],
                    "{}",
                    preset.slug()
                );
            }
        }

        let body = &seen.body;
        assert_eq!(body["model"], "checked-model", "{}", preset.slug());
        match dialect {
            Dialect::Chat => {
                assert_eq!(
                    body["tools"][0]["function"]["name"],
                    "lookup",
                    "{}",
                    preset.slug()
                );
                let messages = body["messages"].as_array().unwrap();
                assert!(messages.iter().any(|message| message["role"] == "user"));
            }
            Dialect::Responses => {
                assert_eq!(body["tools"][0]["name"], "lookup", "{}", preset.slug());
                assert!(body["input"].is_array(), "{}", preset.slug());
            }
            Dialect::Messages => {
                assert!(
                    seen.headers
                        .iter()
                        .any(|(name, _)| name == "anthropic-version"),
                    "{}",
                    preset.slug()
                );
                assert_eq!(body["tools"][0]["name"], "lookup", "{}", preset.slug());
                assert_eq!(body["messages"][0]["role"], "user", "{}", preset.slug());
            }
        }
        dialects.insert(format!("{dialect:?}"));
    }
    // Each of the three dialects is spoken by some preset.
    assert_eq!(dialects.len(), 3, "{dialects:?}");
}
