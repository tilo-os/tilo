use serde_json::{Value, json};

use super::*;
use crate::protocol::{Message, Role, ToolCall, ToolDefinition, ToolResult};
use crate::{
    ModelDescriptor, ModelRequestLimits, ProviderProtocol, ReasoningSetting, TokenLimitParam,
};

fn profile(protocol: ProviderProtocol, endpoint: &str) -> ProviderProfile {
    ProviderProfile {
        endpoint: endpoint.into(),
        ..ProviderProfile::hosted(protocol)
    }
}

fn chat() -> ProviderProfile {
    profile(
        ProviderProtocol::OpenAiCompatible,
        "https://example.test/v1",
    )
}

fn anthropic() -> ProviderProfile {
    profile(ProviderProtocol::Anthropic, "https://api.anthropic.com")
}

fn openai() -> ProviderProfile {
    profile(
        ProviderProtocol::OpenAiCompatible,
        "https://api.openai.com/v1",
    )
}

fn lookup() -> ToolDefinition {
    ToolDefinition {
        name: "lookup".into(),
        description: "Look up a resource".into(),
        input_schema: json!({"type": "object", "properties": {"count": {"type": "integer"}}}),
    }
}

fn call(id: &str, extra: Option<Value>) -> Content {
    Content::ToolCall(ToolCall {
        id: id.into(),
        name: "lookup".into(),
        input: json!({"count": 2}),
        extra_content: extra,
    })
}

fn answered(id: &str) -> Message {
    Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: id.into(),
            content: "found".into(),
            is_error: false,
        })],
    }
}

fn turns() -> ModelRequest {
    ModelRequest {
        messages: vec![
            Message::text(Role::System, "Be brief."),
            Message::text(Role::User, "Weather?"),
            Message {
                role: Role::Assistant,
                content: vec![call("call_1", None)],
            },
            answered("call_1"),
        ],
        tools: vec![lookup()],
        tool_choice: ToolChoice::Auto,
        max_output_tokens: 300,
    }
}

fn plain(protocol: ProviderProtocol) -> ModelRequest {
    ModelRequest {
        messages: vec![
            Message::text(Role::System, "instructions"),
            Message::text(Role::User, "question"),
        ],
        tools: vec![lookup()],
        tool_choice: ToolChoice::Required,
        max_output_tokens: if protocol == ProviderProtocol::Anthropic {
            256
        } else {
            128
        },
    }
}

fn written(profile: &ProviderProfile, request: &ModelRequest, streaming: bool) -> Value {
    body(
        profile,
        "m",
        request,
        Ask {
            streaming,
            extra: None,
        },
    )
    .unwrap()
}

#[test]
fn a_chat_body_carries_tool_turns_signatures_reasoning_and_the_profiles_fields() {
    let body = written(&chat(), &turns(), false);
    assert_eq!(body["model"], "m");
    assert_eq!(body["max_tokens"], 300);
    assert_eq!(
        body["messages"][0],
        json!({"role": "system", "content": "Be brief."})
    );
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["function"]["arguments"],
        "{\"count\":2}"
    );
    assert_eq!(
        body["messages"][3],
        json!({"role": "tool", "tool_call_id": "call_1", "content": "found"})
    );
    assert_eq!(body["tools"][0]["function"]["name"], "lookup");
    assert_eq!(body["tool_choice"], "auto");
    assert!(body.get("stream").is_none());

    let streamed = written(&openai_chat(), &turns(), true);
    assert_eq!(streamed["max_completion_tokens"], 300);
    assert!(streamed.get("max_tokens").is_none());
    assert_eq!(streamed["stream"], true);
    assert_eq!(streamed["stream_options"], json!({"include_usage": true}));

    // A Gemini signature and DeepSeek's reasoning go back as they came.
    let mut request = turns();
    request.messages[2].content = vec![
        Content::Reasoning {
            text: "thinking".into(),
            extra_content: Some(json!({"chat_field": "reasoning_content"})),
        },
        call(
            "call_1",
            Some(json!({"google": {"thought_signature": "signed"}})),
        ),
    ];
    let body = written(&chat(), &request, false);
    assert_eq!(body["messages"][2]["reasoning_content"], "thinking");
    assert_eq!(
        body["messages"][2]["tool_calls"][0]["extra_content"],
        json!({"google": {"thought_signature": "signed"}})
    );

    let mut tuned = chat();
    tuned.token_limit_param = Some(TokenLimitParam::MaxCompletionTokens);
    tuned.reasoning = Some(ReasoningSetting {
        param: crate::ReasoningParam::ReasoningEffort,
        effort: "low".into(),
    });
    tuned.omit_params = vec!["tool_choice".into(), "stream_options".into()];
    let body = written(&tuned, &turns(), true);
    assert_eq!(body["max_completion_tokens"], 300);
    assert_eq!(body["reasoning_effort"], "low");
    assert_eq!(
        body["tool_choice"], "auto",
        "a required field is never omitted"
    );
    assert!(body.get("stream_options").is_none());
}

fn openai_chat() -> ProviderProfile {
    let mut profile = openai();
    profile.openai_api = Some(OpenAiApi::ChatCompletions);
    profile
}

#[test]
fn a_models_allowance_caps_what_is_asked() {
    let mut profile = chat();
    let mut model = ModelDescriptor::new("m");
    model.request_limits = Some(ModelRequestLimits {
        max_output_tokens: 100,
        runtime_context_tokens: None,
        plan_timeout_ms: None,
    });
    profile.models = vec![model];
    assert_eq!(written(&profile, &turns(), false)["max_tokens"], 100);
}

#[test]
fn an_anthropic_body_lifts_the_system_prompt_pairs_tool_blocks_and_marks_its_cache() {
    let body = written(&anthropic(), &turns(), false);
    assert_eq!(
        body["system"],
        json!([{"type": "text", "text": "Be brief.", "cache_control": {"type": "ephemeral"}}])
    );
    assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
    assert_eq!(body["messages"][2]["role"], "user");
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "call_1");
    assert_eq!(
        body["messages"][2]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(body["tool_choice"], json!({"type": "auto"}));
    assert_eq!(body["max_tokens"], 300);

    let forced = written(&anthropic(), &plain(ProviderProtocol::Anthropic), false);
    assert_eq!(forced["tool_choice"], json!({"type": "any"}));
    let refuses = body_for(
        &anthropic(),
        "claude-sonnet-5-5",
        &plain(ProviderProtocol::Anthropic),
    );
    assert_eq!(refuses["tool_choice"], json!({"type": "auto"}));

    // Signed thinking goes back; unsigned thinking stays presentation.
    let mut request = turns();
    request.messages[2].content.insert(
        0,
        Content::Reasoning {
            text: "plan".into(),
            extra_content: Some(json!({"anthropic": {"signature": "sig"}})),
        },
    );
    request.messages[2].content.insert(
        0,
        Content::Reasoning {
            text: "shown only".into(),
            extra_content: None,
        },
    );
    let body = written(&anthropic(), &request, false);
    assert_eq!(
        body["messages"][1]["content"][0],
        json!({"type": "thinking", "thinking": "plan", "signature": "sig"})
    );
    assert_eq!(body["messages"][1]["content"][1]["type"], "tool_use");
}

fn body_for(profile: &ProviderProfile, model: &str, request: &ModelRequest) -> Value {
    body(profile, model, request, Ask::default()).unwrap()
}

#[test]
fn a_responses_body_keeps_instructions_apart_and_replays_reasoning_items() {
    let mut request = turns();
    request
        .messages
        .insert(1, Message::text(Role::System, "context"));
    request.messages[3].content = vec![
        Content::Reasoning {
            text: "summary".into(),
            extra_content: Some(
                json!({"openai_responses": {"id": "rs_1", "encrypted_content": "abc="}}),
            ),
        },
        Content::Reasoning {
            text: "forged".into(),
            extra_content: Some(
                json!({"openai_responses": {"id": "msg_1", "encrypted_content": "abc"}}),
            ),
        },
        call("call_1", Some(json!({"openai_responses": {"id": "fc_1"}}))),
    ];
    let mut reasoning = openai();
    reasoning.reasoning = Some(ReasoningSetting {
        param: crate::ReasoningParam::ReasoningEffort,
        effort: "high".into(),
    });
    let body = written(&reasoning, &request, true);
    assert_eq!(url(&reasoning), "https://api.openai.com/v1/responses");
    assert_eq!(body["instructions"], "Be brief.");
    assert_eq!(
        body["input"][0],
        json!({"role": "system", "content": "context"})
    );
    assert_eq!(body["input"][1]["content"][0]["type"], "input_text");
    assert_eq!(
        body["input"][2],
        json!({"type": "reasoning", "id": "rs_1", "summary": [{"type": "summary_text", "text": "summary"}], "encrypted_content": "abc="})
    );
    assert_eq!(body["input"][3]["type"], "function_call");
    assert_eq!(body["input"][3]["id"], "fc_1");
    assert_eq!(body["input"][3]["call_id"], "call_1");
    assert_eq!(body["input"][4]["type"], "function_call_output");
    assert_eq!(body["max_output_tokens"], 300);
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["reasoning"], json!({"effort": "high"}));
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["tools"][0]["name"], "lookup");
}

#[test]
fn a_chatgpt_body_streams_keeps_nothing_and_names_no_limit() {
    let mut profile = crate::presets::Preset::CHATGPT.profile();
    profile.endpoint = "https://chatgpt.com/backend-api/codex".into();
    assert_eq!(Api::of(&profile), Api::ChatGpt);
    let mut request = turns();
    request
        .messages
        .insert(1, Message::text(Role::System, "more"));
    let mut extra = Map::new();
    extra.insert("temperature".into(), json!(0));
    let body = body(
        &profile,
        "gpt-5-codex",
        &request,
        Ask {
            streaming: false,
            extra: Some(&extra),
        },
    )
    .unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["instructions"], "Be brief.\n\nmore");
    for refused in ["max_output_tokens", "max_tokens", "temperature"] {
        assert!(body.get(refused).is_none(), "{refused}");
    }
}

#[test]
fn requests_no_wire_can_carry_are_refused_before_anything_is_sent() {
    let mut unanswered = turns();
    unanswered.messages.push(answered("call_9"));
    let mut picture_in_system = turns();
    picture_in_system.messages[0].content.push(Content::Image {
        media_type: "image/png".into(),
        base64: "AAAA".into(),
    });
    let mut reused = turns();
    reused.messages.push(Message {
        role: Role::Assistant,
        content: vec![call("call_1", None)],
    });
    for request in [unanswered, picture_in_system, reused] {
        for profile in [chat(), anthropic(), openai()] {
            assert!(body(&profile, "m", &request, Ask::default()).is_err());
        }
    }
    let mut vertex = ProviderProfile::google_vertex("p", "global", "m");
    vertex.endpoint = "https://example.test".into();
    let mut prefill = turns();
    prefill
        .messages
        .push(Message::text(Role::Assistant, "Sure,"));
    assert!(body(&vertex, "m", &prefill, Ask::default()).is_err());
}

#[test]
fn whole_answers_read_text_calls_reasoning_usage_and_stop_reasons() {
    let request = turns();
    let openai = response(
        &chat(),
        &request,
        r#"{"choices":[{"finish_reason":"tool_calls","message":{"content":null,"reasoning_content":"why",
           "tool_calls":[{"id":"c","type":"function","function":{"name":"lookup","arguments":"{\"count\":1}"},
           "extra_content":{"google":{"thought_signature":"s"}}}]}}],
           "usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
    )
    .unwrap();
    assert_eq!(openai.stop_reason, StopReason::ToolCalls);
    let calls = openai.tool_calls().unwrap();
    assert_eq!(calls[0].input, json!({"count": 1}));
    assert_eq!(
        calls[0].extra_content,
        Some(json!({"google": {"thought_signature": "s"}}))
    );
    assert_eq!(
        openai.message.content[0],
        Content::Reasoning {
            text: "why".into(),
            extra_content: Some(json!({"chat_field": "reasoning_content"}))
        }
    );
    assert_eq!(openai.usage.total_tokens, 5);

    let claude = response(
        &anthropic(),
        &request,
        r#"{"role":"assistant","content":[{"type":"thinking","thinking":"t","signature":"sig"},{"type":"text","text":"Hi"}],
           "stop_reason":"max_tokens","usage":{"input_tokens":4,"cache_read_input_tokens":6,"output_tokens":1}}"#,
    )
    .unwrap();
    assert_eq!(claude.stop_reason, StopReason::OutputLimit);
    assert_eq!(claude.usage.input_tokens, 10);
    assert_eq!(claude.usage.cached_input_tokens, 6);
    assert_eq!(
        claude.message.content[0],
        Content::Reasoning {
            text: "t".into(),
            extra_content: Some(json!({"anthropic": {"signature": "sig"}}))
        }
    );

    let responses = response(
        &super::tests::openai(),
        &request,
        r#"{"status":"completed","output":[
            {"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"s"}],"encrypted_content":"abc="},
            {"type":"function_call","id":"fc_1","call_id":"call_2","name":"lookup","arguments":"{}"}],
           "usage":{"input_tokens":9,"output_tokens":4,"input_tokens_details":{"cached_tokens":8},"output_tokens_details":{"reasoning_tokens":3}}}"#,
    )
    .unwrap();
    assert_eq!(responses.stop_reason, StopReason::ToolCalls);
    assert_eq!(responses.usage.cached_input_tokens, 8);
    assert_eq!(responses.usage.reasoning_tokens, 3);
    assert_eq!(
        responses.message.content[0],
        Content::Reasoning {
            text: "s".into(),
            extra_content: Some(
                json!({"openai_responses": {"id": "rs_1", "encrypted_content": "abc="}})
            )
        }
    );
    assert_eq!(
        responses.tool_calls().unwrap()[0].extra_content,
        Some(json!({"openai_responses": {"id": "fc_1"}}))
    );

    let refusal = response(
        &chat(),
        &request,
        r#"{"choices":[{"finish_reason":"stop","message":{"content":null,"refusal":"no"}}]}"#,
    )
    .unwrap();
    assert_eq!(refusal.stop_reason, StopReason::Refusal);
    assert!(refusal.tool_calls().is_err());
}

#[test]
fn malformed_answers_and_undeclared_calls_are_no_answer() {
    let request = turns();
    for (profile, answer) in [
        (anthropic(), "{}"),
        (
            chat(),
            r#"{"choices":[{"message":{"tool_calls":[{"id":"c","function":{"name":"lookup","arguments":"[1]"}}]}}]}"#,
        ),
        (
            chat(),
            r#"{"choices":[{"finish_reason":"surprise","message":{"content":"Hi"}}]}"#,
        ),
        (
            chat(),
            r#"{"choices":[{"message":{"tool_calls":[{"id":"c","function":{"name":"lookup"}}]}}]}"#,
        ),
        (
            chat(),
            r#"{"choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{"id":"c","function":{"name":"delete_all","arguments":"{}"}}]}}]}"#,
        ),
        (
            chat(),
            r#"{"choices":[{"finish_reason":"stop","message":{"content":""}}]}"#,
        ),
        (
            chat(),
            r#"{"choices":[{"finish_reason":"stop","message":{"content":"x","reasoning_content":{"a":1}}}]}"#,
        ),
    ] {
        assert!(
            response(&profile, &request, answer).is_err(),
            "{answer} is no answer"
        );
    }
    let mut none = request;
    none.tool_choice = ToolChoice::None;
    assert!(
        response(
            &chat(),
            &none,
            r#"{"choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{"id":"c","function":{"name":"lookup","arguments":"{}"}}]}}]}"#,
        )
        .is_err(),
        "a call is no answer when no tool was offered"
    );
}

fn sse(events: &[Value], named: bool) -> String {
    use std::fmt::Write as _;
    let mut stream = String::new();
    for event in events {
        if named && let Some(kind) = event.get("type").and_then(Value::as_str) {
            let _ = writeln!(stream, "event: {kind}");
        }
        let _ = write!(stream, "data: {event}\n\n");
    }
    stream
}

fn anthropic_events(arguments: &str) -> Vec<Value> {
    vec![
        json!({"type":"message_start","message":{"id":"m","type":"message","role":"assistant","content":[],"stop_reason":null,"usage":{"input_tokens":5,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Looking 🦀"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call_1","name":"lookup","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":arguments}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ]
}

/// `stream` read on `api` in pieces cut at `cut` bytes.
fn read(
    api: Api,
    stream: &str,
    cut: usize,
    request: &ModelRequest,
) -> (Result<ModelResponse, StreamError>, Vec<Delta>) {
    let mut reader = Stream::new(api);
    let mut deltas = Vec::new();
    for piece in stream.as_bytes().chunks(cut.max(1)) {
        if let Err(error) = reader.push(piece, &mut deltas) {
            return (Err(error), deltas);
        }
    }
    (reader.finish(request, &mut deltas), deltas)
}

#[test]
fn an_anthropic_stream_cut_anywhere_gives_the_same_answer_and_deltas() {
    let stream = sse(&anthropic_events("{\"count\":2}"), true);
    for cut in 1..stream.len() {
        let (answer, deltas) = read(Api::Anthropic, &stream, cut, &turns());
        let answer = answer.unwrap();
        assert_eq!(answer.usage.input_tokens, 5);
        assert_eq!(answer.usage.output_tokens, 7);
        assert_eq!(answer.tool_calls().unwrap()[0].input, json!({"count": 2}));
        assert_eq!(
            deltas,
            vec![
                Delta::Text("Looking 🦀".into()),
                Delta::CallOpened {
                    index: 0,
                    id: Some("call_1".into()),
                    name: "lookup".into()
                },
                Delta::CallArguments {
                    index: 0,
                    json: "{\"count\":2}".into()
                },
                Delta::CallClosed {
                    index: 0,
                    id: "call_1".into(),
                    name: "lookup".into()
                },
            ]
        );
    }
}

#[test]
fn a_stream_that_never_reaches_its_end_or_breaks_its_order_is_no_answer() {
    let events = anthropic_events("{\"count\":2}");
    let full = sse(&events, true);
    // Cut at every event: only the whole stream is an answer.
    for end in 0..events.len() {
        let (answer, _) = read(Api::Anthropic, &sse(&events[..end], true), 7, &turns());
        assert!(answer.is_err(), "{end} events are no answer");
    }
    let undelimited = full.trim_end_matches('\n');
    assert_eq!(
        read(Api::Anthropic, undelimited, 9, &turns())
            .0
            .unwrap_err(),
        StreamError::Truncated
    );
    let mut reordered = events.clone();
    reordered.swap(0, 1);
    assert!(
        read(Api::Anthropic, &sse(&reordered, true), 9, &turns())
            .0
            .is_err()
    );
    let broken = sse(&anthropic_events("{\"count\":"), true);
    assert!(read(Api::Anthropic, &broken, 9, &turns()).0.is_err());
    let mut failed = events[..3].to_vec();
    failed.push(json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}}));
    assert!(matches!(
        read(Api::Anthropic, &sse(&failed, true), 9, &turns()).0,
        Err(StreamError::Provider(_))
    ));
    let mut bytes = full.into_bytes();
    let at = bytes.iter().position(|byte| *byte == 0xF0).unwrap();
    bytes[at + 1] = b'x';
    let mut reader = Stream::new(Api::Anthropic);
    assert!(reader.push(&bytes, &mut Vec::new()).is_err(), "not UTF-8");
}

fn chunk(delta: &Value, finish: &Value) -> Value {
    json!({"object":"chat.completion.chunk","choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
}

#[test]
fn a_chat_stream_assembles_split_names_and_arguments_only_at_its_finish() {
    // Some providers split a name; others restate it whole in every chunk.
    for pieces in [&["look", "up"][..], &["lookup", "lookup", ""][..]] {
        let mut events = vec![chunk(&json!({"reasoning_content":"why"}), &Value::Null)];
        for (at, piece) in pieces.iter().enumerate() {
            let mut fragment = json!({"index":0,"function":{"name":piece,"arguments":""}});
            if at == 0 {
                fragment["id"] = json!("call_1");
                fragment["function"]["arguments"] = json!("{\"count\"");
            }
            events.push(chunk(&json!({"tool_calls":[fragment]}), &Value::Null));
        }
        events.push(chunk(
            &json!({"tool_calls":[{"index":0,"function":{"arguments":":2}"}}]}),
            &Value::Null,
        ));
        events.push(chunk(&json!({}), &json!("tool_calls")));
        events.push(json!({"object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":4,"completion_tokens":3,"total_tokens":7}}));
        let mut stream = sse(&events, false);
        stream.push_str("data: [DONE]\n\n");
        let (answer, deltas) = read(Api::Chat, &stream, 5, &turns());
        let answer = answer.unwrap();
        let calls = answer.tool_calls().unwrap();
        assert_eq!(calls[0].name, "lookup");
        assert_eq!(calls[0].input, json!({"count": 2}));
        assert_eq!(answer.usage.total_tokens, 7);
        assert_eq!(deltas[0], Delta::Reasoning("why".into()));
        assert_eq!(
            deltas[1],
            Delta::CallOpened {
                index: 0,
                id: Some("call_1".into()),
                name: String::new()
            }
        );
        assert_eq!(
            deltas.last(),
            Some(&Delta::CallClosed {
                index: 0,
                id: "call_1".into(),
                name: "lookup".into()
            })
        );
    }
    // An assembled name that was never offered calls nothing.
    let events = [
        chunk(
            &json!({"tool_calls":[{"index":0,"id":"c","function":{"name":"look","arguments":"{}"}}]}),
            &Value::Null,
        ),
        chunk(
            &json!({"tool_calls":[{"index":0,"function":{"name":"upper"}}]}),
            &Value::Null,
        ),
        chunk(&json!({}), &json!("tool_calls")),
    ];
    assert!(
        read(Api::Chat, &sse(&events, false), 3, &turns())
            .0
            .is_err()
    );
    // A stream without its finish was cut.
    assert_eq!(
        read(Api::Chat, &sse(&events[..2], false), 3, &turns())
            .0
            .unwrap_err(),
        StreamError::Truncated
    );
    // A provider failing mid-stream.
    let failed = [
        chunk(&json!({"content":"Hel"}), &Value::Null),
        json!({"error":{"message":"out of credits","code":"insufficient_quota"}}),
    ];
    assert!(matches!(
        read(Api::Chat, &sse(&failed, false), 3, &turns()).0,
        Err(StreamError::Provider(_))
    ));
}

#[test]
fn a_responses_stream_previews_its_items_and_answers_with_its_completed_response() {
    let call = json!({"type":"function_call","id":"fc_1","call_id":"call_2","name":"lookup","arguments":"{\"count\":3}","status":"completed"});
    let mut opened = call.clone();
    opened["arguments"] = json!("");
    let events = [
        json!({"type":"response.created","response":{"status":"in_progress","output":[]}}),
        json!({"type":"response.reasoning_summary_text.delta","delta":"hmm"}),
        json!({"type":"response.output_item.added","output_index":1,"item":opened}),
        json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"count\":3}"}),
        json!({"type":"response.output_item.done","output_index":1,"item":call}),
        json!({"type":"response.completed","response":{"status":"completed","output":[call],"usage":{"input_tokens":2,"output_tokens":1}}}),
    ];
    let (answer, deltas) = read(Api::Responses, &sse(&events, true), 11, &turns());
    let answer = answer.unwrap();
    assert_eq!(answer.tool_calls().unwrap()[0].id, "call_2");
    assert_eq!(
        deltas,
        vec![
            Delta::Reasoning("hmm".into()),
            Delta::CallOpened {
                index: 0,
                id: Some("call_2".into()),
                name: "lookup".into()
            },
            Delta::CallArguments {
                index: 0,
                json: "{\"count\":3}".into()
            },
            Delta::CallClosed {
                index: 0,
                id: "call_2".into(),
                name: "lookup".into()
            },
        ]
    );
    assert_eq!(
        read(Api::Responses, &sse(&events[..5], true), 11, &turns())
            .0
            .unwrap_err(),
        StreamError::Truncated
    );
    let failed = json!({"type":"response.failed","response":{"status":"failed","error":{"code":"server_error"}}});
    assert!(matches!(
        read(Api::ChatGpt, &sse(&[failed], true), 11, &turns()).0,
        Err(StreamError::Provider(_))
    ));
}
