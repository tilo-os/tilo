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
    use std::io::{BufRead as _, Read as _, Write as _};
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
        for bytes in body.as_bytes().chunks(piece) {
            socket.write_all(bytes).unwrap();
        }
    });
    let transport = GatewayHttp {
        client: provider_client().unwrap(),
        endpoint: provider_url(&profile),
        profile: profile.clone(),
        model: "checked-model".into(),
        streaming: true,
        authenticated: true,
        budget: Duration::from_secs(5),
        body: None,
        retry_refusals: false,
        stream_end: Arc::default(),
        failure: Arc::default(),
        status: Arc::default(),
        unary_body: Arc::default(),
    };
    let mut request = CompletionRequest::new("question");
    request.max_tokens = Some(64);
    let mut timing = PlanTiming::start();
    let mut events = Vec::new();
    let mut emit = |event: &Response| {
        events.push(event.clone());
        true
    };
    let response = execute(
        transport,
        &profile,
        Some("secret-never-echo"),
        "checked-model".into(),
        request,
        Some((&mut emit, &mut timing, true)),
    )
    .await
    .map_err(|error| error.why);
    peer.join().unwrap();
    (response, events)
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
