use super::super::*;
use super::*;
use agent_provider_config::protocol as neutral;
use serde_json::json;
use test_http::{Reply, Responder};

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

/// An Anthropic stream that stops after its tool block until the gateway has
/// emitted the call's argument delta, then finishes the turn.
fn anthropic_stream(
    events: Arc<Mutex<Vec<serde_json::Value>>>,
    delta_delivered: std::sync::mpsc::Receiver<()>,
) -> Responder {
    let delta_delivered = Mutex::new(delta_delivered);
    Responder::serve(move |peer| {
        let body: serde_json::Value = serde_json::from_str(&peer.request().body_text()).unwrap();
        assert_eq!(body["stream"], true);
        let _ = peer.write(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        );
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
                delta_delivered
                    .lock()
                    .unwrap()
                    .recv_timeout(tilo_test_env::hang_bound())
                    .expect("the gateway emits the call's argument delta before the turn ends");
                let events = events.lock().unwrap();
                assert_eq!(events.len(), 1);
                assert_eq!(events[0]["event"], EVENT_TOOL_CALL_DELTA);
                assert!(!events.iter().any(|event| event["event"] == EVENT_TOOL_CALL));
                drop(events);
            }
            let _ = peer.write(
                format!(
                    "event: {}\ndata: {}\n\n",
                    event["type"].as_str().unwrap(),
                    event
                )
                .as_bytes(),
            );
        }
    })
}

#[tokio::test(flavor = "current_thread")]
async fn anthropic_streams_deltas_completed_calls_and_usage() {
    let events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let (delta_sent, delta_delivered) = std::sync::mpsc::channel();
    let served = anthropic_stream(events.clone(), delta_delivered);
    let mut profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
    profile.endpoint = served.url();
    let mut timing = PlanTiming::start();
    let mut emit = |event: &Response| {
        events
            .lock()
            .unwrap()
            .push(serde_json::to_value(event).unwrap());
        if event.event.as_deref() == Some(EVENT_TOOL_CALL_DELTA) {
            let _ = delta_sent.send(());
        }
        true
    };
    let response = streamed(
        &profile,
        Some("fabricated-key"),
        &request(),
        &mut emit,
        &mut timing,
        true,
    )
    .await
    .map_err(|failure| failure.why)
    .unwrap();
    assert_eq!(response.usage.input_tokens, 5);
    assert_eq!(response.usage.output_tokens, 7);
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
}

#[tokio::test(flavor = "current_thread")]
async fn each_wire_reaches_a_custom_endpoint_with_the_gateways_policy() {
    for protocol in [
        ProviderProtocol::OpenAiCompatible,
        ProviderProtocol::Anthropic,
    ] {
        let reply = match protocol {
            ProviderProtocol::OpenAiCompatible => json!({"id":"reply","object":"chat.completion","created":1,"model":"checked-model","choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"count\":2}"}}]}}],"usage":{"prompt_tokens":5,"completion_tokens":7,"total_tokens":12}}),
            ProviderProtocol::Anthropic => json!({"id":"reply","type":"message","role":"assistant","model":"checked-model","content":[{"type":"tool_use","id":"call_1","name":"lookup","input":{"count":2}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":7}}),
        }.to_string();
        let served = Responder::start(vec![
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(reply),
        ]);
        let mut profile = ProviderProfile::hosted(protocol);
        profile.endpoint = served.url();
        let response = host_complete(
            &provider_client().unwrap(),
            &profile,
            Some("fabricated-key"),
            "checked-model",
            request(),
            tilo_test_env::hang_bound(),
            crate::openai::Target::Standard,
        )
        .await
        .map_err(|failure| failure.why)
        .unwrap();
        assert_eq!(response.usage.input_tokens, 5);
        assert_eq!(response.usage.output_tokens, 7);
        let calls = response.tool_calls().unwrap();
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].input["count"], 2);
        let seen = served.next_request();
        let body: serde_json::Value = serde_json::from_str(&seen.body_text()).unwrap();
        assert_eq!(body["model"], "checked-model");
        assert_eq!(body["max_tokens"], 256);
        assert!(!body.get("stream").is_some_and(|value| value == true));
        assert_eq!(seen.method, "POST");
        match protocol {
            ProviderProtocol::OpenAiCompatible => {
                assert_eq!(seen.target, "/chat/completions");
                assert_eq!(seen.header("authorization"), Some("Bearer fabricated-key"));
                assert_eq!(body["tools"][0]["function"]["name"], "lookup");
            }
            ProviderProtocol::Anthropic => {
                assert_eq!(seen.target, "/v1/messages");
                assert_eq!(seen.header("x-api-key"), Some("fabricated-key"));
                assert_eq!(body["tools"][0]["name"], "lookup");
                assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
            }
        }
    }
}
