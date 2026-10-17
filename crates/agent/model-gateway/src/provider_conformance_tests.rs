//! The normal, trusted-host and legacy entry points enforce one provider contract.
use super::*;
use serde_json::json;

fn peer(answer: serde_json::Value) -> (ProviderProfile, std::thread::JoinHandle<()>) {
    use std::io::{BufRead as _, Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let profile = ProviderProfile {
        endpoint: format!("http://{}/v1", listener.local_addr().unwrap()),
        ..ProviderProfile::default()
    };
    let task = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = std::io::BufReader::new(&mut socket);
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
        }
        reader.read_exact(&mut vec![0; length]).unwrap();
        let body = answer.to_string();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    (profile, task)
}

async fn invoke(
    path: &str,
    request: neutral::ModelRequest,
    answer: serde_json::Value,
) -> Result<Option<neutral::ModelResponse>, String> {
    let (profile, peer) = peer(answer);
    let client = provider_client().unwrap();
    let result = if path == "legacy" {
        let body = json!({
            "model":"checked-model", "messages":[{"role":"user", "content":"question"}],
            "tools":request.tools.iter().map(|tool| json!({"type":"function", "function": {
                "name":tool.name, "description":tool.description, "parameters":tool.input_schema
            }})).collect::<Vec<_>>(),
            "tool_choice": if request.tool_choice == neutral::ToolChoice::None { "none" } else { "auto" },
            "max_tokens":64
        });
        legacy(
            &client,
            &profile,
            None,
            body,
            Duration::from_secs(5),
            None,
            false,
        )
        .await
        .map(|_| None)
        .map_err(|failure| failure.why)
    } else {
        let request = neutral_request(request).unwrap();
        let result = if path == "host" {
            host_complete(
                &client,
                &profile,
                None,
                "checked-model",
                request,
                Duration::from_secs(5),
                crate::openai::Target::Standard,
            )
            .await
        } else {
            execute(
                GatewayHttp {
                    client,
                    endpoint: provider_url(&profile),
                    profile,
                    model: "checked-model".into(),
                    streaming: false,
                    target: crate::openai::Target::Standard,
                    retry_refusals: false,
                    stream_end: Arc::default(),
                    authenticated: false,
                    budget: Duration::from_secs(5),
                    body: None,
                    failure: Arc::default(),
                    status: Arc::default(),
                    unary_body: Arc::default(),
                    profile_headers: Vec::new(),
                },
                None,
                request,
                None,
            )
            .await
        };
        result.map_err(|failure| failure.why).and_then(|response| {
            neutral_response(response)
                .map(Some)
                .map_err(|error| error.to_string())
        })
    };
    peer.join().unwrap();
    result
}

fn answer(finish: serde_json::Value, tool: bool) -> serde_json::Value {
    let message = if tool {
        json!({"role":"assistant", "content":null, "tool_calls":[{
            "id":"call_1", "type":"function", "function":{"name":"lookup", "arguments":"{\"count\":2}"}
        }]})
    } else {
        json!({"role":"assistant", "content":"answer"})
    };
    let mut response = json!({"id":"reply", "object":"chat.completion", "created":1, "model":"checked-model",
        "choices":[{"index":0, "message":message}],
        "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}});
    response["choices"][0]["finish_reason"] = finish;
    response
}

fn request() -> neutral::ModelRequest {
    let mut request = neutral::Conversation::new("instructions", "question").request(
        vec![neutral::ToolDefinition {
            name: "lookup".into(),
            description: "Look up a resource".into(),
            input_schema: json!({"type":"object"}),
        }],
        64,
    );
    request.tool_choice = neutral::ToolChoice::Auto;
    request
}

#[tokio::test(flavor = "current_thread")]
async fn all_entry_points_reject_unsolicited_tools_and_accept_declared_names() {
    for path in ["neutral", "host", "legacy"] {
        for case in ["empty", "none", "unknown", "declared"] {
            let mut request = request();
            match case {
                "empty" => request.tools.clear(),
                "none" => request.tool_choice = neutral::ToolChoice::None,
                "unknown" => request.tools[0].name = "different".into(),
                _ => {}
            }
            let result = invoke(path, request, answer(json!("tool_calls"), true)).await;
            assert_eq!(
                result.is_ok(),
                case == "declared",
                "{path}/{case}: {result:?}"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_metadata_is_required_and_non_dispatchable_outcomes_survive() {
    for path in ["neutral", "host", "legacy"] {
        let mut missing = answer(serde_json::Value::Null, false);
        missing["choices"][0]
            .as_object_mut()
            .unwrap()
            .remove("finish_reason");
        assert!(invoke(path, request(), missing).await.is_err(), "{path}");
    }
    for path in ["neutral", "host"] {
        for (finish, reason) in [
            ("length", neutral::StopReason::OutputLimit),
            ("content_filter", neutral::StopReason::Refusal),
        ] {
            let response = invoke(path, request(), answer(json!(finish), true))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response.stop_reason, reason, "{path}/{finish}");
            assert!(response.tool_calls().is_err());
        }
    }
}
