//! Subscription sign-ins through the production Rig codec: a `ChatGPT`
//! sign-in speaks Responses to the `ChatGPT` backend for its account, and a
//! Copilot sign-in names the editor GitHub's API requires.
use super::*;
use base64::Engine as _;
use serde_json::json;
use std::fmt::Write as _;

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

/// One request: its lowercased head and its JSON body.
fn read_request(socket: &mut std::net::TcpStream) -> (String, serde_json::Value) {
    use std::io::{BufRead as _, Read as _};
    socket
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut reader = std::io::BufReader::new(socket);
    let mut head = String::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
        head.push_str(&line);
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    (
        head.to_ascii_lowercase(),
        serde_json::from_slice(&body).unwrap(),
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
    use std::io::Write as _;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut profile = agent_provider_config::presets::Preset::ChatGpt.profile();
    profile.endpoint = format!(
        "http://{}/backend-api/codex",
        listener.local_addr().unwrap()
    );
    profile.models = vec![agent_provider_config::ModelDescriptor::new("gpt-5-codex")];
    let token = chatgpt_token("acct-7");
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let (head, body) = read_request(&mut socket);
        assert!(
            head.starts_with("post /backend-api/codex/responses "),
            "{head}"
        );
        assert!(head.contains("chatgpt-account-id: acct-7\r\n"), "{head}");
        assert!(head.contains("originator: tilo\r\n"), "{head}");
        assert!(head.contains("authorization: bearer ey"), "{head}");
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
        let stream = responses_stream("hello from the subscription");
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{stream}",
            stream.len()
        )
        .unwrap();
    });
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
    peer.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn a_copilot_sign_in_names_the_editor_and_speaks_chat() {
    use std::io::Write as _;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut profile = agent_provider_config::presets::Preset::Copilot.profile();
    profile.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let (head, body) = read_request(&mut socket);
        assert!(head.starts_with("post /chat/completions "), "{head}");
        assert!(head.contains("authorization: bearer tid=copilot-token\r\n"));
        assert!(head.contains("copilot-integration-id: vscode-chat\r\n"));
        assert!(head.contains("editor-version: "));
        assert_eq!(body["model"], "gpt-4.1");
        let reply = json!({"id":"r","object":"chat.completion","created":0,"model":"gpt-4.1","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hi"}}]}).to_string();
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        )
        .unwrap();
    });
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
    peer.join().unwrap();
}
