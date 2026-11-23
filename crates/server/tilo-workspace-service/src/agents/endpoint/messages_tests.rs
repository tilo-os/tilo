use serde_json::{Value, json};

use super::*;
use crate::workspaces::person_model::protocol::Usage;

fn asked(body: &Value) -> Asked {
    super::asked(body.to_string().as_bytes()).expect("a Messages request")
}

fn answered(content: Vec<Content>, stop_reason: StopReason, usage: Usage) -> ModelResponse {
    ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason,
        usage,
        cost_micros: 0,
    }
}

/// The events of a stream, each as its name and its data.
fn stream(bytes: &[u8]) -> Vec<(String, Value)> {
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .split("\n\n")
        .filter(|event| !event.is_empty())
        .map(|event| {
            let (name, data) = event.split_once('\n').unwrap();
            (
                name.strip_prefix("event: ").unwrap().to_owned(),
                serde_json::from_str(data.strip_prefix("data: ").unwrap()).unwrap(),
            )
        })
        .collect()
}

tilo_test_env::scenario! {
    fn a_harness_asks_in_its_providers_words_and_the_host_reads_its_own;

    phase a_conversation_with_tools_is_read_turn_by_turn;
    {
        let asked = asked(&json!({
            "model": "claude-opus-5",
            "max_tokens": 32000,
            "stream": true,
            "system": [
                {"type": "text", "text": "You are a coding agent.", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "Work in /work/app."},
            ],
            "tools": [
                {"name": "Bash", "description": "Run a command", "input_schema": {"type": "object",
                    "properties": {"command": {"type": "string"}}}},
                {"type": "web_search_20250305", "name": "web_search"},
                {"type": "custom", "name": "Read"},
            ],
            "tool_choice": {"type": "any"},
            "messages": [
                {"role": "user", "content": "Why did daily use fall?"},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "first the deploys", "signature": "sig"},
                    {"type": "text", "text": "Looking at the deploys."},
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "git log"}},
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": [
                        {"type": "text", "text": "three commits"},
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "aGk="}},
                    ]},
                    {"type": "text", "text": "Also check Tuesday."},
                ]},
            ],
        }));
        assert_eq!((asked.model.as_str(), asked.stream), ("claude-opus-5", true));
        assert_eq!(asked.request.max_output_tokens, 32000);
        assert_eq!(asked.request.tool_choice, ToolChoice::Required);
        assert_eq!(
            asked.request.tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>(),
            ["Bash", "Read"],
            "a tool only the provider runs is not offered to the person's model"
        );
        assert_eq!(
            asked.request.messages,
            [
                Message::text(Role::System, "You are a coding agent.\n\nWork in /work/app."),
                Message::text(Role::User, "Why did daily use fall?"),
                Message {
                    role: Role::Assistant,
                    content: vec![
                        Content::Text { text: "Looking at the deploys.".into() },
                        Content::ToolCall(ToolCall {
                            id: "toolu_1".into(),
                            name: "Bash".into(),
                            input: json!({"command": "git log"}),
                            extra_content: None,
                        }),
                    ],
                },
                Message {
                    role: Role::Tool,
                    content: vec![Content::ToolResult(ToolResult {
                        call_id: "toolu_1".into(),
                        content: "three commits\n[a picture, which follows]".into(),
                        is_error: false,
                    })],
                },
                Message {
                    role: Role::User,
                    content: vec![
                        Content::Text { text: "Also check Tuesday.".into() },
                        Content::Image { media_type: "image/png".into(), base64: "aGk=".into() },
                    ],
                },
            ]
        );
    }

    phase a_plain_request_gets_plain_defaults;
    {
        let asked = asked(&json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "tool_choice": {"type": "any"},
        }));
        assert!(!asked.stream);
        assert_eq!(asked.request.max_output_tokens, DEFAULT_OUTPUT_TOKENS);
        assert_eq!(asked.request.tool_choice, ToolChoice::Auto, "no tools, so none to require");
        assert_eq!(asked.request.messages, [Message::text(Role::User, "hi")]);
    }

    phase what_is_no_model_message_is_refused_in_the_providers_shape;
    {
        for body in [
            &b"not json"[..],
            &b"{\"model\": \"m\"}"[..],
            &b"{\"messages\": [{\"role\": \"system\", \"content\": \"x\"}]}"[..],
        ] {
            let why = super::asked(body).unwrap_err();
            let refused = failure(400, "invalid_request_error", &why);
            let said: Value = serde_json::from_slice(&refused.body).unwrap();
            assert_eq!(said["type"], "error");
            assert_eq!(said["error"]["type"], "invalid_request_error");
            assert!(!said["error"]["message"].as_str().unwrap().is_empty());
        }
    }

    phase a_count_of_tokens_is_answered_without_a_model;
    {
        let body = vec![b'x'; 4000];
        assert_eq!(
            serde_json::from_slice::<Value>(&counted(&body).body).unwrap(),
            json!({"input_tokens": 1000})
        );
    }
}

tilo_test_env::scenario! {
    fn the_hosts_answer_goes_back_in_the_providers_words;

    phase an_answer_is_the_providers_message_with_its_own_count_of_tokens;
    {
        let response = answered(
            vec![
                Content::Reasoning { text: "kept on the host".into(), extra_content: None },
                Content::Text { text: "It is Safari.".into() },
                Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "Bash".into(),
                    input: json!({"command": "git diff"}),
                    extra_content: None,
                }),
            ],
            StopReason::ToolCalls,
            Usage {
                input_tokens: 1200,
                output_tokens: 80,
                total_tokens: 1280,
                cached_input_tokens: 1000,
                cache_write_tokens: 150,
                reasoning_tokens: 0,
            },
        );
        let said = message(&response, "claude-opus-5", "msg_1");
        assert_eq!(
            said,
            json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-5",
                "content": [
                    {"type": "text", "text": "It is Safari."},
                    {"type": "tool_use", "id": "call-1", "name": "Bash", "input": {"command": "git diff"}},
                ],
                "stop_reason": "tool_use",
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 50,
                    "cache_read_input_tokens": 1000,
                    "cache_creation_input_tokens": 150,
                    "output_tokens": 80,
                },
            })
        );
        let whole = whole(&said);
        assert_eq!((whole.status, whole.content_type), (200, "application/json"));
        assert_eq!(serde_json::from_slice::<Value>(&whole.body).unwrap(), said);
    }

    phase the_same_answer_as_a_stream_opens_says_and_closes_each_block;
    {
        let response = answered(
            vec![
                Content::Text { text: "It is Safari.".into() },
                Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "Bash".into(),
                    input: json!({"command": "git diff"}),
                    extra_content: None,
                }),
            ],
            StopReason::ToolCalls,
            Usage { input_tokens: 10, output_tokens: 7, total_tokens: 17, ..Usage::default() },
        );
        let events = stream(&events(&message(&response, "m", "msg_2")));
        assert_eq!(
            events.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
            [
                "message_start",
                "content_block_start", "content_block_delta", "content_block_stop",
                "content_block_start", "content_block_delta", "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        for (name, data) in &events {
            assert_eq!(data["type"], name.as_str(), "an event's data names its event");
        }
        let start = &events[0].1["message"];
        assert_eq!(start["id"], "msg_2");
        assert_eq!(start["content"], json!([]));
        assert_eq!(start["stop_reason"], Value::Null);
        assert_eq!(start["usage"]["input_tokens"], 10);
        assert_eq!(start["usage"]["output_tokens"], 0);
        assert_eq!(events[1].1["content_block"], json!({"type": "text", "text": ""}));
        assert_eq!(events[2].1["delta"], json!({"type": "text_delta", "text": "It is Safari."}));
        assert_eq!(
            events[4].1["content_block"],
            json!({"type": "tool_use", "id": "call-1", "name": "Bash", "input": {}})
        );
        assert_eq!(events[5].1["index"], 1);
        // The tool's input arrives as the text of its JSON, which the harness reads back.
        let partial = events[5].1["delta"]["partial_json"].as_str().unwrap();
        assert_eq!(events[5].1["delta"]["type"], "input_json_delta");
        assert_eq!(serde_json::from_str::<Value>(partial).unwrap(), json!({"command": "git diff"}));
        assert_eq!(events[7].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(events[7].1["usage"]["output_tokens"], 7);
    }

    phase an_answer_of_words_alone_ends_its_turn;
    {
        let response = answered(
            vec![Content::Text { text: "Done.".into() }],
            StopReason::EndTurn,
            Usage::default(),
        );
        let said = message(&response, "m", "msg_3");
        assert_eq!(said["stop_reason"], "end_turn");
        let limit = answered(Vec::new(), StopReason::OutputLimit, Usage::default());
        assert_eq!(message(&limit, "m", "msg_4")["stop_reason"], "max_tokens");
        assert_eq!(message(&limit, "m", "msg_4")["content"], json!([]));
    }

    phase a_refusal_is_said_so_that_only_what_may_pass_is_asked_again;
    {
        let day = ModelError {
            kind: ErrorKind::PermissionDenied,
            message: "Coder has used today's $5 limit.".into(),
        };
        let (status, kind, words) = refusal(&day);
        assert_eq!((status, kind), (403, "permission_error"), "a day's limit ends the turn");
        assert!(words.contains("Coder") && words.contains("$5"), "{words}");
        for (error, status, kind) in [
            (ErrorKind::NoProvider, 404, "not_found_error"),
            (ErrorKind::Authentication, 401, "authentication_error"),
            (ErrorKind::RateLimited, 429, "rate_limit_error"),
            (ErrorKind::TimedOut, 529, "overloaded_error"),
            (ErrorKind::ContextLimit, 400, "invalid_request_error"),
            (ErrorKind::CreditsExhausted, 402, "billing_error"),
        ] {
            let (said_status, said_kind, words) = refusal(&ModelError::new(error));
            assert_eq!((said_status, said_kind), (status, kind), "{error:?}");
            assert!(!words.is_empty());
        }
        let whole = failure(403, "permission_error", &words);
        assert_eq!(
            serde_json::from_slice::<Value>(&whole.body).unwrap(),
            json!({"type": "error", "error": {"type": "permission_error", "message": words}})
        );
        let answered = refused(&day);
        assert_eq!((answered.status, answered.content_type), (403, "application/json"));
    }
}
