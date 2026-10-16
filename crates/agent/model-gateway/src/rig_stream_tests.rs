//! Adversarial wire fixtures run through Rig, not a second SSE decoder.
use super::*;
use agent_provider_config::protocol as neutral;
use serde_json::json;

fn chunk(delta: &serde_json::Value, finish: &serde_json::Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"id":"reply","object":"chat.completion.chunk","created":1,"model":"checked-model","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    )
}

async fn wire(body: String) -> (Result<CompletionResponse, String>, Vec<Response>) {
    wire_pieces(body, ProviderProtocol::OpenAiCompatible, usize::MAX).await
}

async fn wire_pieces(
    body: String,
    protocol: ProviderProtocol,
    piece: usize,
) -> (Result<CompletionResponse, String>, Vec<Response>) {
    let (result, frames, _) = test_stream(body.into_bytes(), protocol, piece, false, true).await;
    (result, frames)
}

pub(in super::super) async fn test_stream(
    body: Vec<u8>,
    protocol: ProviderProtocol,
    piece: usize,
    legacy: bool,
    deltas: bool,
) -> (
    Result<CompletionResponse, String>,
    Vec<Response>,
    PlanTiming,
) {
    test_stream_gated(body, protocol, piece, (legacy, deltas), None).await
}

// A gate makes the provider withhold the rest of the turn until an actual
// gateway preview arrives. Event ordering in a fully buffered reply cannot
// prove that a caller sees progress while generation is still running.
async fn test_stream_gated(
    body: Vec<u8>,
    protocol: ProviderProtocol,
    piece: usize,
    options: (bool, bool),
    gate: Option<usize>,
) -> (
    Result<CompletionResponse, String>,
    Vec<Response>,
    PlanTiming,
) {
    use std::io::{BufRead as _, Read as _, Write as _};
    let (legacy, deltas) = options;
    let (preview_tx, preview_rx) = std::sync::mpsc::channel::<Response>();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut profile = ProviderProfile::hosted(protocol);
    profile.endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let peer = std::thread::spawn(move || {
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
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let split = gate.unwrap_or(body.len());
        for bytes in body[..split].chunks(piece) {
            socket.write_all(bytes).unwrap();
        }
        if gate.is_some() {
            let preview = preview_rx.recv_timeout(Duration::from_secs(3)).expect(
                "argument preview must arrive before the provider releases the terminal frame",
            );
            assert_eq!(preview.event.as_deref(), Some(EVENT_TOOL_CALL_DELTA));
            let value: serde_json::Value =
                serde_json::from_str(preview.output.as_deref().unwrap()).unwrap();
            assert_eq!(value["name"], "");
            assert!(value["id"].is_null());
            assert!(!value["delta"].as_str().unwrap().is_empty());
        }
        for bytes in body[split..].chunks(piece) {
            socket.write_all(bytes).unwrap();
        }
    });
    let transport = GatewayHttp {
        client: provider_client().unwrap(),
        endpoint: provider_url(&profile),
        profile: profile.clone(),
        model: "checked-model".into(),
        streaming: true,
        target: crate::openai::Target::Standard,
        authenticated: true,
        budget: Duration::from_secs(5),
        body: legacy.then(|| json!({"model":"checked-model", "messages":[]})),
        retry_refusals: false,
        stream_end: Arc::default(),
        failure: Arc::default(),
        status: Arc::default(),
        unary_body: Arc::default(),
        profile_headers: Vec::new(),
    };
    let request = stream_test_request();
    let mut timing = PlanTiming::start();
    let mut events = Vec::new();
    let mut emit = |event: &Response| {
        let _ = preview_tx.send(event.clone());
        events.push(event.clone());
        true
    };
    let response = execute(
        transport,
        Some("secret-never-echo"),
        request,
        Some((&mut emit, &mut timing, deltas)),
    )
    .await
    .map_err(|error| error.why);
    peer.join().unwrap();
    (response, events, timing)
}

fn stream_test_request() -> CompletionRequest {
    let mut request = CompletionRequest::new("question");
    request.max_tokens = Some(64);
    request.tools = [
        "lookup",
        "list_operations",
        "list_options",
        "invoke",
        "plan",
    ]
    .into_iter()
    .map(|name| {
        rig_core::completion::ToolDefinition::new(
            message::ToolName::new(name).unwrap(),
            "test tool",
            json!({"type":"object"}),
        )
    })
    .collect();
    request
}

#[tokio::test(flavor = "current_thread")]
async fn cut_and_malformed_streams_keep_text_but_never_authorize_tools() {
    for ending in ["", "data: {broken-secret-never-echo\n\n", "data: {}\n\n"] {
        let body = chunk(&json!({"content":"partial answer"}), &json!(null))
            + &chunk(
                &json!({"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"count\":2}"}}]}),
                &json!(null),
            )
            + ending;
        let (result, events) = wire(body).await;
        let error = result.expect_err("an unterminated turn must fail");
        assert!(!error.contains("secret-never-echo"));
        assert!(
            events
                .iter()
                .any(|event| event.event.as_deref() == Some("text_delta")
                    && event.output.as_deref() == Some("\"partial answer\""))
        );
        assert!(
            !events
                .iter()
                .any(|event| event.event.as_deref() == Some(EVENT_TOOL_CALL))
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn presentation_event_cap_fails_explicitly_with_retained_prefix() {
    for kind in ["content", "reasoning_content"] {
        let mut body = String::new();
        for _ in 0..MAX_DELTA_EVENTS + 17 {
            body.push_str(&chunk(&json!({kind: "🦀"}), &json!(null)));
        }
        body.push_str(&chunk(&json!({}), &json!("stop")));
        body.push_str("data: [DONE]\n\n");
        let (result, events) = wire(body).await;
        assert!(result.unwrap_err().contains("truncated"));
        assert_eq!(events.len(), MAX_DELTA_EVENTS);
        let retained: String = events
            .iter()
            .map(|event| serde_json::from_str::<String>(event.output.as_deref().unwrap()).unwrap())
            .collect();
        assert_eq!(retained, "🦀".repeat(MAX_DELTA_EVENTS));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn reasoning_and_token_limit_are_preserved_without_dispatch() {
    let body = chunk(
        &json!({"reasoning_content":"provider summary"}),
        &json!(null),
    ) + &chunk(&json!({"content":"unfinished"}), &json!(null))
        + &chunk(&json!({}), &json!("length"))
        + "data: [DONE]\n\n";
    let (result, events) = wire(body).await;
    let response = neutral_response(result.unwrap()).unwrap();
    assert_eq!(response.stop_reason, neutral::StopReason::OutputLimit);
    assert!(response.tool_calls().is_err());
    assert!(response.message.content.iter().any(|content| matches!(content, neutral::Content::Reasoning { text } if text == "provider summary")));
    assert!(
        events
            .iter()
            .any(|event| event.event.as_deref() == Some("reasoning_delta"))
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event.as_deref() == Some(EVENT_TOOL_CALL))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn refusal_is_not_a_successful_end_turn_even_with_stop() {
    let body = chunk(&json!({"refusal":"cannot comply"}), &json!(null))
        + &chunk(&json!({}), &json!("stop"))
        + "data: [DONE]\n\n";
    let (result, events) = wire(body).await;
    let response = neutral_response(result.unwrap()).unwrap();
    assert_eq!(response.stop_reason, neutral::StopReason::Refusal);
    assert!(response.tool_calls().is_err());
    assert!(
        !events
            .iter()
            .any(|event| event.event.as_deref() == Some(EVENT_TOOL_CALL))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn fragmented_arguments_dispatch_only_after_successful_finish() {
    for finish in ["tool_calls", "length"] {
        let body = chunk(
            &json!({"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"count\":"}}]}),
            &json!(null),
        ) + &chunk(
            &json!({"tool_calls":[{"index":0,"function":{"arguments":"2}"}}]}),
            &json!(null),
        ) + &chunk(&json!({}), &json!(finish))
            + "data: [DONE]\n\n";
        let (result, events) = wire(body).await;
        let response = neutral_response(result.unwrap()).unwrap();
        if finish == "tool_calls" {
            assert_eq!(response.tool_calls().unwrap()[0].input["count"], 2);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.event.as_deref() == Some(EVENT_TOOL_CALL))
                    .count(),
                1
            );
        } else {
            assert!(response.tool_calls().is_err());
            assert!(
                !events
                    .iter()
                    .any(|event| event.event.as_deref() == Some(EVENT_TOOL_CALL))
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn chat_argument_previews_arrive_while_provider_is_still_generating() {
    for fragmented in [false, true] {
        let arguments = json!({"reply": "live plan text ".repeat(80)}).to_string();
        let prefix = chunk(
            &json!({"tool_calls":[{"index":0,"id":"a","type":"function","function":{
                "name": if fragmented { "" } else { "plan" },
                "arguments": arguments
            }}]}),
            &json!(null),
        );
        let mut body = prefix.clone();
        if fragmented {
            for name in ["p", "l", "an", "", "plan"] {
                body += &chunk(
                    &json!({"tool_calls":[{"index":0,"function":{"name":name}}]}),
                    &json!(null),
                );
            }
        }
        body += &chunk(&json!({}), &json!("tool_calls"));
        body += "data: [DONE]\n\n";
        let (result, frames, _) = test_stream_gated(
            body.into_bytes(),
            ProviderProtocol::OpenAiCompatible,
            1,
            (false, true),
            Some(prefix.len()),
        )
        .await;
        let response = result.unwrap();
        let replay = crate::openai::read_turn(&response.raw).unwrap();
        assert_eq!(replay.calls[0].name, "plan");
        let normalized = neutral_response(response).unwrap();
        assert_eq!(normalized.tool_calls().unwrap()[0].name, "plan");
        assert_eq!(
            frames.first().unwrap().event.as_deref(),
            Some(EVENT_TOOL_CALL_DELTA)
        );
        assert_eq!(
            frames.last().unwrap().event.as_deref(),
            Some(EVENT_TOOL_CALL)
        );
        let previews = frames
            .iter()
            .filter(|frame| frame.event.as_deref() == Some(EVENT_TOOL_CALL_DELTA))
            .map(|frame| {
                let value: serde_json::Value =
                    serde_json::from_str(frame.output.as_deref().unwrap()).unwrap();
                value["delta"].as_str().unwrap().to_owned()
            })
            .collect::<String>();
        assert_eq!(previews, arguments);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn visible_argument_previews_never_authorize_an_unfinished_call() {
    let prefix = chunk(
        &json!({"tool_calls":[{"index":0,"id":"a","type":"function","function":{
            "name":"plan", "arguments": json!({"reply":"unfinished ".repeat(80)}).to_string()
        }}]}),
        &json!(null),
    );
    let (result, frames, _) = test_stream_gated(
        prefix.clone().into_bytes(),
        ProviderProtocol::OpenAiCompatible,
        1,
        (false, true),
        Some(prefix.len()),
    )
    .await;
    assert!(result.is_err());
    assert!(!frames.is_empty());
    assert!(
        frames
            .iter()
            .all(|frame| frame.event.as_deref() == Some(EVENT_TOOL_CALL_DELTA))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn empty_unknown_and_invalid_argument_payloads_never_dispatch() {
    for body in [
        String::new(),
        "data: {}\n\ndata: [DONE]\n\n".into(),
        "data: []\n\ndata: [DONE]\n\n".into(),
        chunk(
            &json!({"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{broken"}}]}),
            &json!("tool_calls"),
        ) + "data: [DONE]\n\n",
    ] {
        let (result, events) = wire(body).await;
        assert!(result.is_err());
        assert!(
            !events
                .iter()
                .any(|event| event.event.as_deref() == Some(EVENT_TOOL_CALL))
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn in_band_credit_failure_keeps_partial_output_without_leaking_provider_fields() {
    let body = chunk(&json!({"content":"partial"}), &json!(null))
        + "data: {\"error\":{\"code\":\"insufficient_quota\",\"message\":\"secret-never-echo\",\"param\":\"secret-never-echo\"}}\n\n";
    let (result, events) = wire(body).await;
    let error = result.err().unwrap();
    assert!(error.contains("credits"));
    assert!(!error.contains("secret-never-echo"));
    assert!(
        events
            .iter()
            .any(|event| event.event.as_deref() == Some("text_delta"))
    );
    assert!(
        !events
            .iter()
            .any(|event| event.event.as_deref() == Some(EVENT_TOOL_CALL))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn undelimited_tail_after_finish_is_not_silently_discarded() {
    let body = chunk(&json!({"content":"partial"}), &json!("stop")) + "data: {cut";
    let (result, events) = wire(body).await;
    assert!(result.err().unwrap().contains("truncated"));
    assert!(
        !events
            .iter()
            .any(|event| event.event.as_deref() == Some(EVENT_TOOL_CALL))
    );
}

fn anthropic_events(arguments: &str) -> Vec<serde_json::Value> {
    vec![
        json!({"type":"message_start","message":{"id":"reply","type":"message","role":"assistant","model":"checked-model","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_1","name":"lookup","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":arguments}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ]
}

fn anthropic_sse(events: &[serde_json::Value]) -> String {
    use std::fmt::Write as _;
    events.iter().fold(String::new(), |mut text, event| {
        write!(
            text,
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        )
        .unwrap();
        text
    })
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_byte_cuts_preserve_arguments_usage_and_authorized_calls() {
    let input = json!({"query":"é漢字","enabled":false});
    let events = anthropic_events(&input.to_string());
    for piece in [1, 2, 3, 7, 64, usize::MAX] {
        let (result, frames) =
            wire_pieces(anthropic_sse(&events), ProviderProtocol::Anthropic, piece).await;
        let response = result.unwrap();
        assert_eq!(response.usage.input_tokens, Some(5));
        assert_eq!(response.usage.output_tokens, Some(7));
        let response = neutral_response(response).unwrap();
        assert_eq!(response.tool_calls().unwrap()[0].input, input);
        let calls = frames
            .iter()
            .filter(|frame| frame.event.as_deref() == Some(EVENT_TOOL_CALL))
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1);
        let call: serde_json::Value =
            serde_json::from_str(calls[0].output.as_deref().unwrap()).unwrap();
        assert_eq!(call["id"], "call_1");
        assert_eq!(call["arguments"], input.to_string());
        let delta = frames
            .iter()
            .filter(|frame| frame.event.as_deref() == Some(EVENT_TOOL_CALL_DELTA))
            .map(|frame| {
                let value: serde_json::Value =
                    serde_json::from_str(frame.output.as_deref().unwrap()).unwrap();
                value["delta"].as_str().unwrap().to_owned()
            })
            .collect::<String>();
        assert_eq!(delta, input.to_string());
        assert_eq!(
            frames.last().unwrap().event.as_deref(),
            Some(EVENT_TOOL_CALL)
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_cut_error_and_malformed_turns_never_authorize_finished_blocks() {
    let events = anthropic_events("{\"count\":2}");
    let mut broken = Vec::new();
    // A closing tool block or stop reason alone is not a complete envelope.
    for end in [1, 3, 4, 5] {
        broken.push(anthropic_sse(&events[..end]));
    }
    broken.push(anthropic_sse(&events[..4]) + "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"secret-never-echo\"}}\n\n");
    broken.push(anthropic_sse(&events[..4]) + "event: message_delta\ndata: {cut");
    broken.push(anthropic_sse(&anthropic_events("{broken")));
    broken.push(anthropic_sse(&events[1..]));
    let mut duplicate = events.clone();
    duplicate.insert(1, events[0].clone());
    broken.push(anthropic_sse(&duplicate));
    let mut stray = events.clone();
    stray[2]["index"] = json!(9);
    broken.push(anthropic_sse(&stray));
    for (index, body) in broken.into_iter().enumerate() {
        let (result, frames) = wire_pieces(body, ProviderProtocol::Anthropic, 3).await;
        let error = result.expect_err(&format!("malformed fixture {index} must fail"));
        assert!(!error.contains("secret-never-echo"));
        assert!(
            !frames
                .iter()
                .any(|frame| frame.event.as_deref() == Some(EVENT_TOOL_CALL)),
            "fixture {index} authorized work"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_reasoning_history_and_ordered_calls_use_the_real_decoder() {
    let body = chunk(&json!({"reasoning_content":"provider "}), &json!(null))
        + &chunk(&json!({"reasoning_content":"thought"}), &json!(null))
        + &chunk(
            &json!({"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"list_operations","arguments":"{\"x\":"}}]}),
            &json!(null),
        )
        + &chunk(
            &json!({"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}),
            &json!(null),
        )
        + &chunk(
            &json!({"tool_calls":[{"index":1,"id":"b","type":"function","function":{"name":"plan","arguments":"{}"}}]}),
            &json!(null),
        )
        + &chunk(&json!({}), &json!("tool_calls"))
        + "data: {\"choices\":[],\"usage\":{\"completion_tokens\":3}}\n\ndata: [DONE]\n\n";
    for deltas in [false, true] {
        let (result, frames, _) = test_stream(
            body.clone().into_bytes(),
            ProviderProtocol::OpenAiCompatible,
            1,
            true,
            deltas,
        )
        .await;
        let response = result.unwrap();
        let replay = crate::openai::read_turn(&response.raw).unwrap();
        assert_eq!(replay.message["reasoning_content"], "provider thought");
        assert_eq!(replay.calls[0].name, "list_operations");
        assert_eq!(replay.calls[0].input, json!({"x":1}));
        assert_eq!(replay.calls[1].id, "b");
        assert!(replay.text.is_none());
        assert_eq!(response.usage.output_tokens, Some(3));
        let calls = frames
            .iter()
            .filter(|frame| frame.event.as_deref() == Some(EVENT_TOOL_CALL))
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(calls[0].output.as_deref().unwrap()).unwrap()
                ["name"],
            "list_operations"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(calls[1].output.as_deref().unwrap()).unwrap()
                ["name"],
            "plan"
        );
        assert_eq!(
            frames
                .iter()
                .any(|frame| frame.event.as_deref() == Some(EVENT_TOOL_CALL_DELTA)),
            deltas
        );
        assert!(frames.iter().all(|frame| {
            !frame
                .output
                .as_deref()
                .unwrap()
                .contains("provider thought")
        }));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn fragmented_names_agree_in_emitted_normalized_and_replay_calls() {
    // Two declared names share list_op: neither the decoder nor the adapter
    // may guess which one was intended before the remaining fragments arrive.
    let fragments = [
        json!({"index":0,"id":"a","type":"function","function":{"name":"","arguments":"{"}}),
        json!({"index":1,"id":"b","type":"function","function":{"arguments":"{"}}),
        json!({"index":0,"function":{"name":"list_"}}),
        json!({"index":1,"function":{"name":"lo"}}),
        json!({"index":0,"function":{"name":"op"}}),
        json!({"index":1,"function":{"name":"ok"}}),
        json!({"index":0,"function":{"name":"er"}}),
        json!({"index":1,"function":{"name":"up","arguments":"}"}}),
        json!({"index":0,"function":{"name":"ations","arguments":"}"}}),
        json!({"index":1,"function":{"name":"lookup"}}),
        json!({"index":0,"function":{"name":"list_operations"}}),
        json!({"index":0,"function":{"name":""}}),
    ];
    let mut body = String::new();
    for fragment in fragments {
        body += &chunk(&json!({"tool_calls":[fragment]}), &json!(null));
    }
    body += &chunk(&json!({}), &json!("tool_calls"));
    body += "data: [DONE]\n\n";
    for legacy in [false, true] {
        for deltas in [false, true] {
            let (result, frames, _) = test_stream(
                body.clone().into_bytes(),
                ProviderProtocol::OpenAiCompatible,
                1,
                legacy,
                deltas,
            )
            .await;
            let response = result.unwrap();
            let replay = crate::openai::read_turn(&response.raw)
                .unwrap_or_else(|error| panic!("{error}: {}", response.raw));
            let normalized = neutral_response(response).unwrap();
            let normalized = normalized.tool_calls().unwrap();
            let emitted = frames
                .iter()
                .filter(|frame| frame.event.as_deref() == Some(EVENT_TOOL_CALL))
                .map(|frame| {
                    serde_json::from_str::<serde_json::Value>(frame.output.as_deref().unwrap())
                        .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(normalized.len(), 2);
            assert_eq!(replay.calls.len(), 2);
            assert_eq!(emitted.len(), 2);
            for ((id, name), call) in [("a", "list_operations"), ("b", "lookup")]
                .into_iter()
                .zip(normalized)
            {
                assert_eq!(call.name, name);
                let replay = replay.calls.iter().find(|call| call.id == id).unwrap();
                assert_eq!(replay.name, name);
                assert_eq!(replay.input, json!({}));
                let emitted = emitted.iter().find(|call| call["id"] == id).unwrap();
                assert_eq!(emitted["name"], name);
                assert_eq!(emitted["arguments"], "{}");
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_fragmented_tool_names_never_authorize_the_wrong_operation() {
    for final_name in ["op", "delete_everything", "unknown"] {
        let body = chunk(
            &json!({"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"list_","arguments":"{"}}]}),
            &json!(null),
        ) + &chunk(
            &json!({"tool_calls":[{"index":0,"function":{"name":final_name,"arguments":"}"}}]}),
            &json!("tool_calls"),
        ) + "data: [DONE]\n\n";
        let (result, frames) = wire(body).await;
        assert!(result.is_err(), "an undeclared assembled name must fail");
        assert!(
            frames
                .iter()
                .all(|frame| frame.event.as_deref() != Some(EVENT_TOOL_CALL))
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_reasoning_utf8_and_hostile_error_values_fail_closed() {
    let mut invalid_utf8 = chunk(&json!({"content":"partial"}), &json!(null)).into_bytes();
    invalid_utf8.extend_from_slice(
        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"\xff\"}}]}\n\n",
    );
    invalid_utf8.extend_from_slice(chunk(&json!({}), &json!("stop")).as_bytes());
    invalid_utf8.extend_from_slice(b"data: [DONE]\n\n");
    let mut bodies = vec![
        invalid_utf8,
        (chunk(
            &json!({"reasoning_content":{"wrong":"type"}}),
            &json!("stop"),
        ) + "data: [DONE]\n\n")
            .into_bytes(),
    ];
    for error in [
        json!(null),
        json!(false),
        json!([]),
        json!("private-provider-content"),
        json!({"message":"private-provider-content","type":"🦀"}),
    ] {
        bodies.push(
            (chunk(&json!({"content":"partial"}), &json!(null))
                + &format!("data: {}\n\n", json!({"error":error}))
                + &chunk(&json!({}), &json!("stop"))
                + "data: [DONE]\n\n")
                .into_bytes(),
        );
    }
    for body in bodies {
        let (result, frames, _) =
            test_stream(body, ProviderProtocol::OpenAiCompatible, 1, false, true).await;
        let why = result.expect_err("malformed provider data must not complete a turn");
        assert!(!why.contains("private-provider-content"));
        assert!(
            frames
                .iter()
                .all(|frame| frame.event.as_deref() != Some(EVENT_TOOL_CALL))
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unfinished_reasoning_records_content_free_progress() {
    let body = chunk(&json!({"reasoning_content":"private"}), &json!(null))
        + &chunk(&json!({"reasoning_content":"💡"}), &json!(null))
        + &chunk(&json!({"reasoning_content":""}), &json!(null))
        + "data: {\"choices\":[{\"index\":1,\"delta\":{\"reasoning_content\":\"foreign\"}}]}\n\n"
        + "data: {\"usage\":{\"completion_tokens\":17},\"choices\":[]}\n\n";
    let (result, _, timing) = test_stream(
        body.into_bytes(),
        ProviderProtocol::OpenAiCompatible,
        1,
        true,
        false,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(timing.reasoning_chunks, 2);
    assert_eq!(timing.reasoning_bytes, 11);
    assert!(timing.first_reasoning.is_some());
    assert!(timing.last_stream >= timing.last_reasoning);
    assert!(timing.completion_tokens.is_none());
    let fields = timing.progress_log_fields();
    assert!(fields.contains("reasoning_chunks=2 reasoning_bytes=11"));
    for private in ["private", "💡", "reasoning_content"] {
        assert!(!fields.contains(private));
    }
}

#[test]
fn sse_tail_handles_split_crlf_comments_and_complete_frames() {
    let mut tail = SseTail::default();
    for bytes in [b"data: {}\r".as_slice(), b"\n\r", b"\n: heartbeat\r\n\r\n"] {
        tail.push(bytes);
    }
    assert!(!tail.incomplete());
    tail.push(b"data: {cut");
    assert!(tail.incomplete());
}

#[test]
fn provider_diagnostics_use_fixed_advice_not_untrusted_identifiers() {
    for status in [400, 401, 402, 403, 429, 500] {
        let advice = provider_error_advice(status, br#"{"error":{"message":"secret-never-echo","code":"secret-never-echo","type":"secret-never-echo","param":"secret-never-echo"}}"#);
        assert!(!advice.contains("secret-never-echo"));
    }
    assert!(
        provider_error_advice(400, br#"{"error":{"code":"context_length_exceeded"}}"#)
            .contains("context limit")
    );
    assert!(
        provider_error_advice(429, br#"{"error":{"code":"insufficient_quota"}}"#)
            .contains("credits")
    );
}
