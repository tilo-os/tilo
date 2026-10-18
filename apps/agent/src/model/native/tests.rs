//! Deterministic queued-request tests: no model or network is involved.
use super::*;

#[test]
fn queued_turn_cannot_borrow_a_newer_direction_lease() {
    let (sent, received) = std::sync::mpsc::channel();
    let scope = Scope::new(17, "lease:0".into(), move |run, lease, body, _| {
        sent.send((run, lease, body)).unwrap();
        Ok(Value::Null)
    });
    let turn = scope.snapshot();
    scope.set_lease("lease:1".into());
    // Model work deliberately starts only after steering has advanced the
    // live scope. The frozen sender must still claim the original frontier.
    let body = tilo_model_gateway::Request::NativeTransform {
        invocation: Value::Null,
        profile_id: "profile".into(),
        model_id: "model".into(),
        instructions: "old prompt".into(),
        input: "old arguments".into(),
        max_output_tokens: 32,
        timeout_ms: 1000,
    };
    turn.request(body, Duration::from_secs(1)).unwrap();
    let (run, lease, body) = received.recv().unwrap();
    assert_eq!(run, 17);
    assert_eq!(lease, "lease:0");
    assert!(body.to_string().contains("old arguments"));
    assert_eq!(*scope.snapshot().lease.lock_recover(), "lease:1");
}

#[test]
fn sibling_scopes_and_reply_channels_are_independent() {
    let (sent_a, replies_a) = std::sync::mpsc::channel();
    let (sent_b, replies_b) = std::sync::mpsc::channel();
    let a = Scope::new(1, "A:0".into(), move |run, lease, _, _| {
        sent_a.send((run, lease)).unwrap();
        Ok(json!({"task":"A"}))
    });
    let b = Scope::new(2, "B:0".into(), move |run, lease, _, _| {
        sent_b.send((run, lease)).unwrap();
        Ok(json!({"task":"B"}))
    });
    a.set_lease("A:1".into());
    let frozen_b = b.snapshot();
    drop(a);
    let reply = (frozen_b.send)(
        frozen_b.run_id,
        frozen_b.lease.lock_recover().clone(),
        Value::Null,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(reply, json!({"task":"B"}));
    assert_eq!(replies_b.recv().unwrap(), (2, "B:0".into()));
    assert!(replies_a.try_recv().is_err());
}

fn prose_frame(kind: &str, payload: Value) -> Value {
    json!({"ok": true, "event": kind, "output": payload.to_string()})
}

#[test]
fn buffered_native_replay_joins_only_adjacent_prose_and_keeps_terminal_output() {
    use agent_provider_config::protocol::{Message, Role, StopReason, ToolChoice, Usage};
    let response = ModelResponse {
        message: Message::text(Role::Assistant, "Hello world!"),
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
    };
    let reply = json!({
        "events": [
            prose_frame("text_delta", json!("Hello ")),
            prose_frame("text_delta", json!("world")),
            prose_frame("reasoning_delta", json!("Visible ")),
            prose_frame("reasoning_delta", json!("summary")),
            prose_frame("tool_call_delta", json!({"index":0,"id":"a","name":"read","delta":"{}"})),
            prose_frame("text_delta", json!("!")),
            prose_frame("tool_call", json!({"id":"a"})),
            prose_frame("text_delta", json!(" After tool")),
        ],
        "response": {"ok": true, "output": serde_json::to_string(&response).unwrap()}
    });
    let scope = Scope::new(1, "lease".into(), move |_, _, _, _| Ok(reply.clone()));
    let mut events = Vec::new();
    let actual = scope
        .complete(
            "profile",
            "model",
            ModelRequest {
                messages: vec![],
                tools: vec![],
                tool_choice: ToolChoice::Auto,
                max_output_tokens: 100,
            },
            Duration::from_secs(1),
            &mut |event| events.push(event.clone()),
        )
        .unwrap();
    assert_eq!(actual, response);
    assert_eq!(
        events,
        vec![
            StreamEvent::TextDelta {
                text: "Hello world".into()
            },
            StreamEvent::ReasoningDelta {
                text: "Visible summary".into()
            },
            StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("a".into()),
                name: Some("read".into()),
                arguments: "{}".into()
            },
            StreamEvent::TextDelta { text: "!".into() },
            StreamEvent::TextDelta {
                text: " After tool".into()
            },
            StreamEvent::Finished { response },
        ]
    );
}

#[test]
fn buffered_native_gateway_delivers_one_packet_with_terminal_result() {
    use crate::model::{GatewayTurns, MarkGate, TurnEvent, TurnRequest, TurnTransport};
    use agent_provider_config::protocol::{Message, Role, StopReason, ToolChoice, Usage};
    let response = ModelResponse {
        message: Message::text(Role::Assistant, "Answer"),
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
    };
    let reply = json!({
        "events": [
            prose_frame("text_delta", json!("An")),
            prose_frame("text_delta", json!("swer")),
            prose_frame("reasoning_delta", json!("Visible summary")),
            prose_frame("tool_call_delta", json!({"index":0,"id":"a","name":"read","delta":"{}"})),
        ],
        "response": {"ok":true,"output":serde_json::to_string(&response).unwrap()}
    });
    let transport = GatewayTurns {
        native: Some(Scope::new(1, "lease".into(), move |_, _, _, _| {
            Ok(reply.clone())
        })),
        profile_id: "profile".into(),
        model_id: "model".into(),
        marks: Arc::new(MarkGate::default()),
    };
    let rx = transport.start(TurnRequest {
        body: serde_json::to_value(ModelRequest {
            messages: vec![],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: 100,
        })
        .unwrap(),
        timeout: Duration::from_secs(1),
        trace: None,
    });
    let TurnEvent::Buffered(events) = rx.recv_timeout(Duration::from_secs(1)).unwrap() else {
        panic!("native replay must arrive atomically, not fragment by fragment");
    };
    assert!(matches!(events.last(), Some(TurnEvent::Finished(Ok(body)))
        if serde_json::from_str::<ModelResponse>(body).unwrap() == response));
    assert!(events.contains(&TurnEvent::Neutral(StreamEvent::TextDelta {
        text: "Answer".into()
    })));
    assert!(
        events.contains(&TurnEvent::Neutral(StreamEvent::ReasoningDelta {
            text: "Visible summary".into()
        }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, TurnEvent::Frame { .. })),
        "tool previews retained"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, TurnEvent::Neutral(StreamEvent::Finished { .. })))
            .count(),
        1
    );
    assert!(
        rx.recv_timeout(Duration::from_secs(1)).is_err(),
        "no second presentation packet"
    );
}

#[test]
fn buffered_native_failure_flushes_partial_private_prose_before_failure() {
    use agent_provider_config::protocol::ToolChoice;
    let reply = json!({"events": [
        prose_frame("reasoning_delta", json!("Partial ")),
        prose_frame("reasoning_delta", json!("summary")),
        {"ok": false, "error": "unavailable"},
    ]});
    let scope = Scope::new(1, "lease".into(), move |_, _, _, _| Ok(reply.clone()));
    let mut events = Vec::new();
    assert!(
        scope
            .complete(
                "profile",
                "model",
                ModelRequest {
                    messages: vec![],
                    tools: vec![],
                    tool_choice: ToolChoice::Auto,
                    max_output_tokens: 100,
                },
                Duration::from_secs(1),
                &mut |event| events.push(event.clone())
            )
            .is_err()
    );
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0],
        StreamEvent::ReasoningDelta {
            text: "Partial summary".into()
        }
    );
    assert!(matches!(events[1], StreamEvent::Failed { .. }));
}
