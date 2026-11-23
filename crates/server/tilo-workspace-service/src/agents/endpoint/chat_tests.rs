use serde_json::{Value, json};

use super::*;
use crate::workspaces::person_model::protocol::Usage;

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

tilo_test_env::scenario! {
    fn a_program_asks_in_chat_completions_and_the_host_reads_its_own;

    phase a_conversation_with_tools_is_read_message_by_message;
    {
        let asked = asked(json!({
            "model": "gpt-5",
            "stream": true,
            "max_completion_tokens": 900,
            "tool_choice": "required",
            "tools": [{"type": "function", "function": {
                "name": "read_file", "description": "Read a file",
                "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
            }}],
            "messages": [
                {"role": "developer", "content": "You fix code."},
                {"role": "user", "content": [
                    {"type": "text", "text": "What is in main.rs?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBOR"}},
                    {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}},
                ]},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call-1", "type": "function",
                     "function": {"name": "read_file", "arguments": "{\"path\":\"main.rs\"}"}},
                    {"id": "call-2", "type": "function",
                     "function": {"name": "read_file", "arguments": "{\"path\":\"lib.rs\"}"}},
                ]},
                {"role": "tool", "tool_call_id": "call-1", "content": "fn main() {}"},
                {"role": "tool", "tool_call_id": "call-2", "content": [{"type": "text", "text": "pub mod a;"}]},
            ],
        }).to_string().as_bytes()).unwrap();
        assert_eq!((asked.model.as_str(), asked.stream), ("gpt-5", true));
        assert_eq!(asked.request.max_output_tokens, 900);
        assert_eq!(asked.request.tool_choice, ToolChoice::Required);
        assert_eq!(
            asked.request.tools,
            [ToolDefinition {
                name: "read_file".into(),
                description: "Read a file".into(),
                input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            }]
        );
        let call = |id: &str, path: &str| Content::ToolCall(ToolCall {
            id: id.into(),
            name: "read_file".into(),
            input: json!({"path": path}),
            extra_content: None,
        });
        let result = |id: &str, text: &str| Content::ToolResult(ToolResult {
            call_id: id.into(),
            content: text.into(),
            is_error: false,
        });
        assert_eq!(
            asked.request.messages,
            [
                Message::text(Role::System, "You fix code."),
                Message {
                    role: Role::User,
                    content: vec![
                        Content::Text { text: "What is in main.rs?".into() },
                        Content::Image { media_type: "image/png".into(), base64: "iVBOR".into() },
                        Content::Text { text: NOT_PASSED.into() },
                    ],
                },
                Message { role: Role::Assistant, content: vec![call("call-1", "main.rs"), call("call-2", "lib.rs")] },
                Message {
                    role: Role::Tool,
                    content: vec![result("call-1", "fn main() {}"), result("call-2", "pub mod a;")],
                },
            ]
        );
    }

    phase a_plain_request_gets_plain_defaults_and_nonsense_is_refused;
    {
        let plain = asked(br#"{"model": "m", "max_tokens": 12, "messages": [{"role": "user", "content": "hi"}]}"#).unwrap();
        assert!(!plain.stream);
        assert_eq!(plain.request.max_output_tokens, 12);
        assert_eq!(plain.request.tool_choice, ToolChoice::Auto);
        assert_eq!(plain.request.messages, [Message::text(Role::User, "hi")]);
        let bare = asked(br#"{"messages": [{"role": "user", "content": "hi"}]}"#).unwrap();
        assert_eq!(bare.request.max_output_tokens, DEFAULT_OUTPUT_TOKENS);
        for body in [
            &b"not json"[..],
            &br#"{"model": "m"}"#[..],
            &br#"{"messages": []}"#[..],
            &br#"{"messages": [{"role": "narrator", "content": "x"}]}"#[..],
        ] {
            assert!(asked(body).is_err(), "{}", String::from_utf8_lossy(body));
        }
    }
}

tilo_test_env::scenario! {
    fn the_hosts_answer_goes_back_as_a_chat_completion;

    phase an_answer_is_a_completion_with_its_calls_and_count_of_tokens;
    {
        let response = answered(
            vec![
                Content::Text { text: "Reading it.".into() },
                Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    input: json!({"path": "main.rs"}),
                    extra_content: None,
                }),
            ],
            StopReason::ToolCalls,
            Usage {
                input_tokens: 1200,
                output_tokens: 80,
                total_tokens: 1280,
                cached_input_tokens: 1000,
                cache_write_tokens: 0,
                reasoning_tokens: 30,
            },
        );
        assert_eq!(
            completion(&response, "gpt-5", "chatcmpl-1", 7),
            json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "created": 7,
                "model": "gpt-5",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Reading it.",
                        "tool_calls": [{
                            "id": "call-1", "type": "function",
                            "function": {"name": "read_file", "arguments": "{\"path\":\"main.rs\"}"},
                        }],
                    },
                    "finish_reason": "tool_calls",
                }],
                "usage": {
                    "prompt_tokens": 1200,
                    "completion_tokens": 80,
                    "total_tokens": 1280,
                    "prompt_tokens_details": {"cached_tokens": 1000},
                    "completion_tokens_details": {"reasoning_tokens": 30},
                },
            })
        );
        let limit = answered(Vec::new(), StopReason::OutputLimit, Usage::default());
        let cut = completion(&limit, "m", "chatcmpl-2", 0);
        assert_eq!(cut["choices"][0]["finish_reason"], "length");
        assert_eq!(cut["choices"][0]["message"], json!({"role": "assistant", "content": null}));
    }

    phase the_same_answer_as_a_stream_is_chunks_ended_by_done;
    {
        let response = answered(
            vec![
                Content::Text { text: "Reading it.".into() },
                Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    input: json!({"path": "main.rs"}),
                    extra_content: None,
                }),
            ],
            StopReason::ToolCalls,
            Usage { input_tokens: 10, output_tokens: 7, total_tokens: 17, ..Usage::default() },
        );
        let stream = String::from_utf8(chunks(&response, "m", "chatcmpl-3", 1)).unwrap();
        let events: Vec<&str> = stream
            .split("\n\n")
            .filter(|event| !event.is_empty())
            .map(|event| event.strip_prefix("data: ").unwrap())
            .collect();
        assert_eq!(events.last(), Some(&"[DONE]"));
        let chunks: Vec<Value> = events[..events.len() - 1]
            .iter()
            .map(|chunk| serde_json::from_str(chunk).unwrap())
            .collect();
        for chunk in &chunks {
            assert_eq!(chunk["object"], "chat.completion.chunk");
            assert_eq!(chunk["id"], "chatcmpl-3");
        }
        let deltas: Vec<&Value> = chunks.iter().map(|chunk| &chunk["choices"][0]["delta"]).collect();
        assert_eq!(deltas[0]["role"], "assistant");
        assert_eq!(deltas[1]["content"], "Reading it.");
        assert_eq!(deltas[2]["tool_calls"][0]["index"], 0);
        assert_eq!(deltas[2]["tool_calls"][0]["function"]["arguments"], "{\"path\":\"main.rs\"}");
        let last = chunks.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(last["usage"]["completion_tokens"], 7);
    }

    phase a_refusal_is_said_so_that_only_what_may_pass_is_asked_again;
    {
        let day = ModelError {
            kind: ErrorKind::PermissionDenied,
            message: "Coder has used today's $5 limit.".into(),
        };
        let answered = refused(&day);
        assert_eq!(answered.status, 403, "a day's limit ends the turn");
        let said: Value = serde_json::from_slice(&answered.body).unwrap();
        assert_eq!(said["error"]["message"], "Coder has used today's $5 limit.");
        assert_eq!(said["error"]["type"], "permission_denied");
        for (error, status) in [
            (ErrorKind::NoProvider, 404),
            (ErrorKind::Authentication, 401),
            (ErrorKind::RateLimited, 429),
            (ErrorKind::TimedOut, 503),
            (ErrorKind::ContextLimit, 400),
            (ErrorKind::CreditsExhausted, 402),
        ] {
            assert_eq!(refusal(&ModelError::new(error)).0, status, "{error:?}");
        }
    }
}
