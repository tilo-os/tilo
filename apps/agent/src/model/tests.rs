//! The model layer's tests: what a request carries, how a turn validates,
//! and what a cut-off answer does to the conversation that continues it.

use super::*;
use crate::plan::Instruction;
use crate::surface::Offer;
use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::{ActionContract, ActionParam, ExecutionLocus, OperationKind, ParamType};

#[test]
fn managed_conversations_use_neutral_requests_and_ordered_results() {
    use agent_provider_config::protocol::{Content, Message, ModelResponse, Role, StopReason};
    let mut conversation = Conversation::neutral("system", "user");
    let tools = tools::typed_loop_definitions();
    let request = conversation.body(&tools);
    assert!(request.get("model").is_none());
    assert!(request.get("max_tokens").is_none());
    assert_eq!(
        request["max_output_tokens"],
        agent_provider_config::ModelDescriptor::DEFAULT_OUTPUT_TOKENS,
        "a model that declared no limit gets the default, never a fixed constant"
    );
    assert_eq!(request["tools"][3]["name"], tools::INVOKE);
    let response = ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(ToolCall {
                id: "call-a".into(),
                name: tools::LIST_APPLICATIONS.into(),
                input: serde_json::json!({}),
                extra_content: None,
            })],
        },
        stop_reason: StopReason::ToolCalls,
        usage: agent_provider_config::protocol::Usage::default(),
        cost_micros: 0,
    };
    let turn = conversation
        .turn(&serde_json::json!(response).to_string())
        .unwrap();
    assert_eq!(turn.calls[0].id, "call-a");
    conversation
        .push(
            turn.assistant,
            vec![("call-a".into(), "visible applications".into())],
        )
        .unwrap();
    let next = conversation.body(&tools);
    assert_eq!(next["messages"][3]["content"][0]["call_id"], "call-a");
    assert!(
        conversation
            .turn(&serde_json::json!(response).to_string())
            .is_err(),
        "consumed call ids cannot be replayed"
    );
}

/// A model that declared its limit is asked for exactly that, and a
/// cut-off answer is carried into the next request with the instruction to
/// go on — never into the history.
#[test]
fn a_conversation_asks_the_models_declared_output_limit_and_carries_a_cut_off_answer() {
    let tools = tools::typed_loop_definitions();
    let mut conversation = Conversation::neutral("system", "user");
    assert_eq!(
        conversation.output_tokens(),
        agent_provider_config::ModelDescriptor::DEFAULT_OUTPUT_TOKENS
    );
    conversation = conversation.budgeted_by(&agent_provider_config::ModelDescriptor {
        max_output_tokens: Some(8_000),
        ..agent_provider_config::ModelDescriptor::new("declares")
    });
    assert_eq!(conversation.output_tokens(), 8_000);
    assert_eq!(conversation.body(&tools)["max_output_tokens"], 8_000);
    assert_eq!(conversation.len(), 2, "the cut-off answer is not history");
    conversation.continue_from("Half an answer ");
    let resumed = conversation.body(&tools);
    let messages = resumed["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 4, "the cut-off answer and its instruction");
    assert_eq!(messages[2]["role"], "assistant");
    assert_eq!(messages[2]["content"][0]["text"], "Half an answer ");
    assert!(
        messages[3]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cut off at the model's output limit")
    );
    assert_eq!(conversation.len(), 2, "still not history");
    conversation.continued();
    assert_eq!(
        conversation.body(&tools)["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn neutral_history_preserves_catalogue_blocks_and_authenticated_steering() {
    let first = FirstTurn {
        operations: "stable catalogue".into(),
        task: "private instruction".into(),
    };
    let mut conversation = Conversation::neutral_first("system", &first);
    conversation.push_user("authenticated direction");
    let request = conversation.body(&tools::typed_loop_definitions());
    assert_eq!(
        request["messages"][1]["content"][0]["text"],
        "stable catalogue"
    );
    assert_eq!(
        request["messages"][1]["content"][1]["text"],
        "private instruction"
    );
    assert_eq!(request["messages"][2]["role"], "user");
    assert_eq!(
        request["messages"][2]["content"][0]["text"],
        "authenticated direction"
    );
    assert_eq!(
        request["tools"][3]["input_schema"]["properties"][tools::INVOKE_STANDALONE]["type"],
        "boolean"
    );
}

#[test]
fn neutral_stream_previews_retain_names_and_never_supply_calls() {
    use agent_provider_config::protocol::StreamEvent;
    let mut previews = NeutralPreviews::default();
    let first = previews
        .frame(&StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("a".into()),
            name: Some(tools::INVOKE.into()),
            arguments: "{\"operation\":\"write\",".into(),
        })
        .unwrap();
    let next = previews
        .frame(&StreamEvent::ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments: "\"parameters\":{\"text\":\"preview\"}}".into(),
        })
        .unwrap();
    let mut scanners = BTreeMap::new();
    let mut saw_preview = false;
    for frame in [first, next] {
        stream_plan_delta(&mut scanners, &frame, &mut |signal| {
            if matches!(
                signal,
                StreamSignal::CallArgument {
                    text: "preview",
                    ..
                }
            ) {
                saw_preview = true;
            }
        });
    }
    assert!(saw_preview);
    assert!(
        previews
            .frame(&StreamEvent::Failed {
                error: agent_provider_config::protocol::ModelError::invalid("reset"),
            })
            .is_none()
    );
    assert!(Conversation::neutral("system", "user").streams());
}

#[test]
fn live_tool_transcript_selects_an_arbitrary_typed_app_action() {
    let offer = Offer {
        app: "calculator".to_string(),
        broker_peer_id: None,
        broker_provider: None,
        descriptor: ActionDescriptor {
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new("calculator.range:main", "sample"),
            resource_kind: "calculator.range".into(),
            resource_label: "Main range".into(),
            action_label: "Sample values".into(),
            keywords: Vec::new(),
            icon: None,
            description: Some("Generate a bounded sample".into()),
            locus: ExecutionLocus::ClientLocal,
            kind: OperationKind::Document,
            contract: ActionContract::default(),
            input: InputSpec::None,
            params: vec![
                ActionParam::required("count", ParamType::Integer, "Number of values"),
                ActionParam::required("normalise", ParamType::Boolean, "Normalise output"),
            ],
            view: None,
        },
    };
    let catalogue = Catalogue {
        offers: vec![offer],
        facts: Vec::new(),
        held: vec!["calculator"],
    };
    let mut revealed = Revealed::new();
    let encoded = serde_json::to_string(&tools::definitions(4)).unwrap();
    assert!(encoded.contains(tools::LIST_OPERATIONS));
    assert!(!encoded.contains("turn the bullets into a table"));

    let mut conversation = Conversation::neutral("system", "sample twelve values");
    let discovery = serde_json::json!({
        "message": {"role": "assistant", "content": [{
            "type": "tool_call",
            "id": "tool-1",
            "name": tools::LIST_OPERATIONS,
            "input": {"application": "calculator"}
        }]},
        "stop_reason": "tool_calls"
    })
    .to_string();
    let turn = conversation.turn(&discovery).unwrap();
    let listed = tools::answer(
        &turn.calls[0].name,
        &turn.calls[0].input,
        &catalogue,
        &mut revealed,
    );
    assert!(listed.contains("calculator.range:main#sample"));
    conversation
        .push(turn.assistant, vec![("tool-1".into(), listed)])
        .unwrap();

    let final_turn = serde_json::json!({
        "message": {"role": "assistant", "content": [{
            "type": "tool_call",
            "id": "tool-2",
            "name": tools::PLAN,
            "input": {
                "steps": [{
                    "operation": "calculator.range:main#sample",
                    "arguments": {"count": 12, "normalise": true}
                }],
                "why": "the calculator publishes the requested sampler",
                "then": null
            }
        }]},
        "stop_reason": "tool_calls"
    })
    .to_string();
    let turn = conversation.turn(&final_turn).unwrap();
    let answer = tools::read_plan(&turn.calls[0].input, &revealed, 4).unwrap();
    let instruction = Instruction::from_answer(
        &answer.actions[0],
        "sample twelve values",
        Some(&answer.arguments[0]),
        &catalogue.offers,
    )
    .unwrap();
    assert_eq!(
        instruction,
        Instruction::Perform {
            id: "calculator.range:main#sample".into(),
            arguments: serde_json::Map::from_iter([
                ("count".into(), serde_json::json!(12)),
                ("normalise".into(), serde_json::json!(true)),
            ]),
            confirmation: crate::plan::ConfirmationEvidence::None,
        }
    );
}

/// `Finished` goes out before the marks a turn held. The next turn may
/// start in between: it sends them before taking the channel, and the
/// finished turn's late release then leaves the channel to it.
#[test]
fn a_late_release_never_frees_the_channel_a_later_turn_holds() {
    let gate = MarkGate::default();
    let trace = tilo_model_client::PlanTrace { id: 1, turn: 0 };
    let held = |gate: &MarkGate| {
        let state = gate.state.lock().unwrap();
        (state.busy, state.pending.len())
    };
    let first = gate.acquire();
    gate.mark(trace, tilo_model_client::TraceEvent::ActionSubmitted);
    assert_eq!(
        held(&gate),
        (true, 1),
        "held while the turn owns the channel"
    );
    // The loop already heard `Finished` and starts the next turn before
    // the first one's thread released.
    gate.plan_over(first);
    let second = gate.acquire();
    assert_eq!(held(&gate), (true, 0), "sent before the next Plan starts");
    gate.release(first);
    assert_eq!(
        held(&gate),
        (true, 0),
        "the next turn still owns the channel"
    );
    gate.mark(trace, tilo_model_client::TraceEvent::ActionSettled);
    assert_eq!(held(&gate), (true, 1));
    // A turn whose Plan is not over (abandoned, still streaming) keeps
    // its marks from the channel when another turn starts.
    let third = gate.acquire();
    assert_eq!(held(&gate), (true, 1));
    gate.release(second);
    assert_eq!(held(&gate), (true, 1));
    gate.plan_over(third);
    gate.release(third);
    assert_eq!(held(&gate), (false, 0), "released: sent, channel free");
}

/// A task stopped while its planning turn streams lets go of the turn.
/// The turn's provider call used to run to its end, billed, holding the
/// agent's one gateway channel from the next task's turns: dropping its
/// [`TurnStop`] now has the gateway stop it.
#[cfg(unix)]
#[test]
fn a_turn_the_loop_lets_go_of_is_stopped_at_the_gateway() {
    use std::os::fd::AsRawFd as _;
    use tilo_model_client::{Frame, Request, Response, read_frame, write_frame};
    let reply = |output: String| Response {
        ok: true,
        output: Some(output),
        configured: None,
        error: None,
        model_error: None,
        event: None,
    };
    let (app, mut gateway) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(
        tilo_model_client::SERVICE_FD_ENV,
        app.as_raw_fd().to_string(),
    );
    let (started_by, started) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        gateway
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let frame: Frame<Request> = read_frame(&mut gateway).unwrap();
        assert!(matches!(frame.value, Request::CancellationSupported));
        write_frame(&mut gateway, &reply("cancel-v1".into())).unwrap();
        let frame: Frame<Request> = read_frame(&mut gateway).unwrap();
        let Request::Cancellable { id, request } = frame.value else {
            panic!("a loop turn is a cancellable operation");
        };
        assert!(matches!(*request, Request::Complete { .. }));
        started_by.send(()).unwrap();
        // The provider works on; nothing comes until the stop.
        let frame: Frame<Request> = read_frame(&mut gateway).unwrap();
        let Request::Cancel { id: stopped } = frame.value else {
            panic!("the abandoned turn is stopped");
        };
        write_frame(&mut gateway, &reply(format!("cancelled:{stopped}"))).unwrap();
        stopped == id
    });
    let turns = GatewayTurns {
        profile_id: "hosted".into(),
        model_id: "model".into(),
        marks: Arc::default(),
    };
    let body = serde_json::to_value(
        agent_provider_config::protocol::Conversation::new("system", "hello")
            .request(Vec::new(), 64),
    )
    .unwrap();
    let stop = TurnStop::default();
    let rx = turns.start(TurnRequest {
        body,
        timeout: Duration::from_secs(30),
        trace: None,
        stop: Arc::clone(&stop.0),
    });
    tilo_test_env::recv("the turn reaching the gateway", &started);
    drop(stop);
    let finished = loop {
        if let TurnEvent::Finished(result) = tilo_test_env::recv("the turn's end", &rx) {
            break result;
        }
    };
    assert!(finished.is_err(), "a stopped turn has no answer");
    assert!(
        server.join().unwrap(),
        "the stop names the turn's operation"
    );
    drop(env);
}
