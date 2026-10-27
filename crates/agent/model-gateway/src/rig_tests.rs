use super::*;
use agent_provider_config::protocol as neutral;
use serde_json::json;

fn request() -> neutral::ModelRequest {
    neutral::ModelRequest {
        messages: vec![
            neutral::Message::text(neutral::Role::System, "instructions"),
            neutral::Message::text(neutral::Role::User, "question"),
        ],
        tools: vec![neutral::ToolDefinition {
            name: "lookup".into(),
            description: "Look up a resource".into(),
            input_schema: json!({"type":"object", "properties":{"count":{"type":"integer"}}}),
        }],
        tool_choice: neutral::ToolChoice::Required,
        max_output_tokens: 256,
    }
}

#[test]
fn neutral_history_preserves_tool_ids_names_and_typed_arguments() {
    let mut ask = request();
    ask.messages.extend([
        neutral::Message {
            role: neutral::Role::Assistant,
            content: vec![neutral::Content::ToolCall(neutral::ToolCall {
                id: "call_1".into(),
                name: "lookup".into(),
                input: json!({"count":2,"enabled":false}),
                extra_content: None,
            })],
        },
        neutral::Message {
            role: neutral::Role::Tool,
            content: vec![neutral::Content::ToolResult(neutral::ToolResult {
                call_id: "call_1".into(),
                content: "result".into(),
                is_error: true,
            })],
        },
    ]);
    let converted = neutral_request(ask).unwrap();
    let message::Message::Assistant(turn) = &converted.chat_history[2] else {
        panic!()
    };
    let message::AssistantContent::ToolCall(call) = &turn.content[0] else {
        panic!()
    };
    assert_eq!(call.id.to_string(), "call_1");
    assert_eq!(call.function.arguments["count"], 2);
    assert_eq!(call.function.arguments["enabled"], false);
    let message::Message::User { content } = &converted.chat_history[3] else {
        panic!()
    };
    let message::UserContent::ToolResult(result) = &content[0] else {
        panic!()
    };
    assert_eq!(result.name.to_string(), "lookup");
    assert!(result.is_error);
}

#[test]
fn mismatched_tool_results_and_wrong_role_content_are_refused_locally() {
    let mut ask = request();
    ask.messages.push(neutral::Message {
        role: neutral::Role::Tool,
        content: vec![neutral::Content::ToolResult(neutral::ToolResult {
            call_id: "unanswered".into(),
            content: "result".into(),
            is_error: false,
        })],
    });
    assert!(neutral_request(ask).is_err());
    let mut ask = request();
    ask.messages[0].content.push(neutral::Content::Image {
        media_type: "image/png".into(),
        base64: "AAAA".into(),
    });
    assert!(neutral_request(ask).is_err());
}

fn spawn_anthropic_peer(
    listener: std::net::TcpListener,
    pending_events: Arc<Mutex<Vec<serde_json::Value>>>,
    delta_received: std::sync::mpsc::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    use std::io::{BufRead as _, Read as _, Write as _};
    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
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
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["stream"], true);
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        for (index, event) in [
            json!({"type":"message_start","message":{"id":"reply","type":"message","role":"assistant","model":"checked-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_1","name":"lookup","input":{}}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"count\":2}"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":7}}),
            json!({"type":"message_stop"}),
        ].into_iter().enumerate() {
            if index == 4 {
                // The tool block is closed, but the turn has not finished.
                // Observe the real emitter before permitting a valid finish.
                delta_received.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                let events = pending_events.lock().unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(events[0]["event"], EVENT_TOOL_CALL_DELTA);
                assert!(!events.iter().any(|event| event["event"] == EVENT_TOOL_CALL));
                drop(events);
            }
            write!(
                socket,
                "event: {}\ndata: {}\n\n",
                event["type"].as_str().unwrap(),
                event
            )
            .unwrap();
        }
    })
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_streams_deltas_completed_calls_and_usage_through_rig() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
    profile.endpoint = format!("http://{}", listener.local_addr().unwrap());
    let events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let pending_events = events.clone();
    let (delta_sent, delta_received) = std::sync::mpsc::channel();
    let peer = spawn_anthropic_peer(listener, pending_events, delta_received);
    let transport = GatewayHttp {
        client: provider_client().unwrap(),
        endpoint: provider_url(&profile),
        profile: profile.clone(),
        model: "checked-model".into(),
        streaming: true,
        api: None,
        target: crate::openai::Target::Standard,
        authenticated: true,
        budget: tilo_test_env::hang_bound(),
        body: None,
        retry_refusals: false,
        stream_end: Arc::default(),
        failure: Arc::default(),
        status: Arc::default(),
        unary_body: Arc::default(),
        profile_headers: Vec::new(),
    };
    let mut timing = PlanTiming::start();
    let mut emit = |event: &Response| {
        events
            .lock()
            .unwrap()
            .push(serde_json::to_value(event).unwrap());
        if event.event.as_deref() == Some(EVENT_TOOL_CALL_DELTA) {
            delta_sent.send(()).unwrap();
        }
        true
    };
    let response = execute(
        transport,
        Some("fabricated-key"),
        neutral_request(request()).unwrap(),
        Some((&mut emit, &mut timing, true)),
    )
    .await
    .map_err(|failure| failure.why)
    .unwrap();
    assert_eq!(response.usage.input_tokens, Some(5));
    assert_eq!(response.usage.output_tokens, Some(7));
    let response = neutral_response(response).unwrap();
    assert_eq!(response.tool_calls().unwrap()[0].id, "call_1");
    assert_eq!(response.tool_calls().unwrap()[0].input["count"], 2);
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["event"], EVENT_TOOL_CALL_DELTA);
    let delta: serde_json::Value =
        serde_json::from_str(events[0]["output"].as_str().unwrap()).unwrap();
    assert_eq!(delta["id"], "call_1");
    assert_eq!(delta["delta"], "{\"count\":2}");
    assert_eq!(events[1]["event"], EVENT_TOOL_CALL);
    let completed: serde_json::Value =
        serde_json::from_str(events[1]["output"].as_str().unwrap()).unwrap();
    drop(events);
    assert_eq!(
        completed,
        json!({"id":"call_1","name":"lookup","arguments":"{\"count\":2}"})
    );
    peer.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_unary_tool_envelopes_keep_their_exact_bytes() {
    use std::io::{BufRead as _, Read as _, Write as _};
    for (protocol, reply) in [
        (
            ProviderProtocol::OpenAiCompatible,
            r#"{"choices":[{"message":{"role":"assistant","tool_calls":[{"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"count\":2}"}}]},"finish_reason":"tool_calls"}]}"#,
        ),
        (
            ProviderProtocol::Anthropic,
            r#"{"type":"message","role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"lookup","input":{"count":2}}],"stop_reason":"tool_use"}"#,
        ),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut profile = ProviderProfile::hosted(protocol);
        profile.endpoint = format!("http://{}", listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
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
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        });
        let mut body = json!({"model":"checked-model", "max_tokens":256, "messages":[{"role":"user","content":"question"}]});
        body["tools"] = if protocol == ProviderProtocol::OpenAiCompatible {
            json!([{"type":"function", "function":{"name":"lookup", "parameters":{"type":"object"}}}])
        } else {
            json!([{"name":"lookup", "input_schema":{"type":"object"}}])
        };
        let response = legacy(
            &provider_client().unwrap(),
            &profile,
            Some("fabricated-key"),
            body,
            tilo_test_env::hang_bound(),
            None,
            true,
        )
        .await
        .map_err(|failure| failure.why)
        .unwrap();
        assert_eq!(response, reply);
        peer.join().unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn rig_wires_reach_custom_endpoints_without_changing_gateway_policy() {
    use std::io::{BufRead as _, Read as _, Write as _};
    for protocol in [
        ProviderProtocol::OpenAiCompatible,
        ProviderProtocol::Anthropic,
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut socket);
            let mut headers = String::new();
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
                headers.push_str(&line);
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let reply = match protocol {
                ProviderProtocol::OpenAiCompatible => json!({"id":"reply","object":"chat.completion","created":1,"model":"checked-model","choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"count\":2}"}}]}}],"usage":{"prompt_tokens":5,"completion_tokens":7,"total_tokens":12}}),
                ProviderProtocol::Anthropic => json!({"id":"reply","type":"message","role":"assistant","model":"checked-model","content":[{"type":"tool_use","id":"call_1","name":"lookup","input":{"count":2}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":7}}),
            }.to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
            (headers, body)
        });
        let mut profile = ProviderProfile::hosted(protocol);
        profile.endpoint = endpoint;
        let transport = GatewayHttp {
            client: provider_client().unwrap(),
            endpoint: provider_url(&profile),
            profile: profile.clone(),
            model: "checked-model".into(),
            streaming: false,
            api: None,
            target: crate::openai::Target::Standard,
            authenticated: true,
            budget: tilo_test_env::hang_bound(),
            body: None,
            retry_refusals: false,
            stream_end: Arc::default(),
            failure: Arc::default(),
            status: Arc::default(),
            unary_body: Arc::default(),
            profile_headers: Vec::new(),
        };
        let response = execute(
            transport,
            Some("fabricated-key"),
            neutral_request(request()).unwrap(),
            None,
        )
        .await
        .map_err(|failure| failure.why)
        .unwrap();
        assert_eq!(response.usage.input_tokens, Some(5));
        assert_eq!(response.usage.output_tokens, Some(7));
        let response = neutral_response(response).unwrap();
        let calls = response.tool_calls().unwrap();
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].input["count"], 2);
        let (headers, body) = peer.join().unwrap();
        assert_eq!(body["model"], "checked-model");
        assert_eq!(body["max_tokens"], 256);
        assert!(!body.get("stream").is_some_and(|value| value == true));
        match protocol {
            ProviderProtocol::OpenAiCompatible => {
                assert!(headers.starts_with("POST /chat/completions "));
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer fabricated-key")
                );
                assert_eq!(body["tools"][0]["function"]["name"], "lookup");
            }
            ProviderProtocol::Anthropic => {
                assert!(headers.starts_with("POST /v1/messages "));
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("x-api-key: fabricated-key")
                );
                assert_eq!(body["tools"][0]["name"], "lookup");
                assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
            }
        }
    }
}
