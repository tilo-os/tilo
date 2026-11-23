//! `OpenAI`'s chat completions, as a program asks its model in them.
//!
//! A program built on an `OpenAI` client posts to `/v1/chat/completions` and
//! reads a `chat.completion`, or a stream of `chat.completion.chunk` events
//! ended by `[DONE]`. This module is the two translations and nothing else: a
//! request in that shape into the host's own [`ModelRequest`], and the host's
//! [`ModelResponse`] or refusal back into that shape.
//!
//! What does not cross: reasoning, a picture named only by an address (it is
//! not fetched), and the model's name, which the program is told back as it
//! said it: the person's choice answers, not the program's.

use serde_json::{Map, Value, json};

use super::{Answer, Asked, DEFAULT_OUTPUT_TOKENS, EVENTS, JSON, NOT_PASSED};
use crate::workspaces::person_model::protocol::{
    Content, ErrorKind, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason,
    ToolCall, ToolChoice, ToolDefinition, ToolResult,
};

/// A failure in chat completions' shape: `kind` is its own word for it.
#[must_use]
pub fn failure(status: u16, kind: &str, words: &str) -> Answer {
    Answer {
        status,
        content_type: JSON,
        body: json!({"error": {"message": words, "type": kind, "param": null, "code": kind}})
            .to_string()
            .into_bytes(),
    }
}

/// Why the workspace's model did not answer, as a chat completions client reads it: only what
/// may pass by itself (a rate, an overloaded provider) is said with a status it asks again for.
#[must_use]
pub fn refusal(error: &ModelError) -> (u16, &'static str) {
    match error.kind {
        ErrorKind::Authentication => (401, "invalid_api_key"),
        ErrorKind::PermissionDenied | ErrorKind::Refused => (403, "permission_denied"),
        ErrorKind::CreditsExhausted => (402, "insufficient_quota"),
        ErrorKind::NoProvider => (404, "model_not_found"),
        ErrorKind::ContextLimit => (400, "context_length_exceeded"),
        ErrorKind::InvalidRequest | ErrorKind::Unsupported => (400, "invalid_request_error"),
        ErrorKind::RateLimited => (429, "rate_limit_exceeded"),
        ErrorKind::Unavailable | ErrorKind::Transport | ErrorKind::TimedOut => {
            (503, "server_error")
        }
        _ => (500, "server_error"),
    }
}

/// Why the workspace's model did not answer, as a chat completions error.
#[must_use]
pub fn refused(error: &ModelError) -> Answer {
    let (status, kind) = refusal(error);
    failure(status, kind, &error.message)
}

/// A request in chat completions' shape, as the host asks its model.
///
/// # Errors
/// Why the request is no chat completion request, in words.
pub fn asked(body: &[u8]) -> Result<Asked, String> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| format!("the request is not JSON: {error}"))?;
    let object = value.as_object().ok_or("the request is not an object")?;
    let turns = object
        .get("messages")
        .and_then(Value::as_array)
        .filter(|turns| !turns.is_empty())
        .ok_or("the request has no messages")?;
    let mut messages: Vec<Message> = Vec::new();
    for turn in turns {
        said(turn, &mut messages)?;
    }
    let tools: Vec<ToolDefinition> = object
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.iter().filter_map(tool).collect())
        .unwrap_or_default();
    let tool_choice = match object.get("tool_choice") {
        _ if tools.is_empty() => ToolChoice::Auto,
        Some(Value::String(choice)) if choice == "none" => ToolChoice::None,
        Some(Value::String(choice)) if choice == "required" => ToolChoice::Required,
        Some(Value::Object(_)) => ToolChoice::Required,
        _ => ToolChoice::Auto,
    };
    let max_output_tokens = object
        .get("max_completion_tokens")
        .or_else(|| object.get("max_tokens"))
        .and_then(Value::as_u64)
        .map_or(DEFAULT_OUTPUT_TOKENS, |tokens| {
            u32::try_from(tokens).unwrap_or(u32::MAX)
        });
    Ok(Asked {
        request: ModelRequest {
            messages,
            tools,
            tool_choice,
            max_output_tokens,
        },
        model: object
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        stream: object
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// The words of a content that is text, or a list of parts of which the text ones are taken.
fn words(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// A picture part the program sent inline as a `data:` address; one it only names by another
/// address is not fetched, and said so.
fn picture(part: &Map<String, Value>) -> Content {
    let inline = part
        .get("image_url")
        .and_then(|image| image.get("url"))
        .and_then(Value::as_str)
        .and_then(|url| url.strip_prefix("data:"))
        .and_then(|data| data.split_once(";base64,"));
    match inline {
        Some((media_type, base64)) => Content::Image {
            media_type: media_type.to_owned(),
            base64: base64.to_owned(),
        },
        None => Content::Text {
            text: NOT_PASSED.to_owned(),
        },
    }
}

/// One message of the program's conversation, added to `messages` as the host writes one: the
/// results of one round of tools are one message of their own.
fn said(turn: &Value, messages: &mut Vec<Message>) -> Result<(), String> {
    let role = turn.get("role").and_then(Value::as_str);
    let content = turn.get("content");
    match role {
        Some("system" | "developer") => {
            let text = words(content);
            if !text.is_empty() {
                messages.push(Message::text(Role::System, text));
            }
        }
        Some("user") => {
            let parts = match content {
                Some(Value::String(text)) if !text.is_empty() => {
                    vec![Content::Text { text: text.clone() }]
                }
                Some(Value::String(_)) => Vec::new(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(Value::as_object)
                    .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                        Some("text") => part
                            .get("text")
                            .and_then(Value::as_str)
                            .filter(|text| !text.is_empty())
                            .map(|text| Content::Text {
                                text: text.to_owned(),
                            }),
                        Some("image_url") => Some(picture(part)),
                        _ => Some(Content::Text {
                            text: NOT_PASSED.to_owned(),
                        }),
                    })
                    .collect(),
                _ => return Err("a user message's content is text or a list of parts".into()),
            };
            if !parts.is_empty() {
                messages.push(Message {
                    role: Role::User,
                    content: parts,
                });
            }
        }
        Some("assistant") => {
            let mut parts = Vec::new();
            let text = words(content);
            if !text.is_empty() {
                parts.push(Content::Text { text });
            }
            for call in turn
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let function = call.get("function");
                let field = |name: &str| {
                    function
                        .and_then(|function| function.get(name))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                };
                parts.push(Content::ToolCall(ToolCall {
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    name: field("name").to_owned(),
                    input: match field("arguments") {
                        "" => json!({}),
                        arguments => serde_json::from_str(arguments).map_err(|error| {
                            format!("a tool call's arguments are not JSON: {error}")
                        })?,
                    },
                    extra_content: None,
                }));
            }
            if !parts.is_empty() {
                messages.push(Message {
                    role: Role::Assistant,
                    content: parts,
                });
            }
        }
        Some("tool") => {
            let result = Content::ToolResult(ToolResult {
                call_id: turn
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                content: words(content),
                is_error: false,
            });
            match messages.last_mut() {
                Some(last) if last.role == Role::Tool => last.content.push(result),
                _ => messages.push(Message {
                    role: Role::Tool,
                    content: vec![result],
                }),
            }
        }
        other => {
            return Err(format!(
                "a message's role is system, developer, user, assistant or tool, not {other:?}"
            ));
        }
    }
    Ok(())
}

/// A function the program runs itself.
fn tool(tool: &Value) -> Option<ToolDefinition> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    let function = tool.get("function")?;
    Some(ToolDefinition {
        name: function.get("name")?.as_str()?.to_owned(),
        description: function
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        input_schema: function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object"})),
    })
}

fn finish_reason(stop: StopReason) -> &'static str {
    match stop {
        StopReason::EndTurn => "stop",
        StopReason::ToolCalls => "tool_calls",
        StopReason::OutputLimit => "length",
        StopReason::Refusal => "content_filter",
    }
}

fn text_of(response: &ModelResponse) -> Option<String> {
    let text: String = response
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!text.is_empty()).then_some(text)
}

fn calls_of(response: &ModelResponse) -> Vec<Value> {
    response
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            Content::ToolCall(call) => Some(json!({
                "id": call.id,
                "type": "function",
                "function": {"name": call.name, "arguments": call.input.to_string()},
            })),
            _ => None,
        })
        .collect()
}

fn usage(response: &ModelResponse) -> Value {
    let usage = response.usage;
    json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.input_tokens.saturating_add(usage.output_tokens),
        "prompt_tokens_details": {"cached_tokens": usage.cached_input_tokens},
        "completion_tokens_details": {"reasoning_tokens": usage.reasoning_tokens},
    })
}

/// The model's answer as the completion `id`, said to be of `model`, made at `created`.
#[must_use]
pub fn completion(response: &ModelResponse, model: &str, id: &str, created: u64) -> Value {
    let mut message = json!({"role": "assistant", "content": text_of(response)});
    let calls = calls_of(response);
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason(response.stop_reason),
        }],
        "usage": usage(response),
    })
}

/// The stream of chunks that says `response`, a whole answer: its role, its words, each tool
/// call, its finish and its count of tokens, then `[DONE]`.
#[must_use]
pub fn chunks(response: &ModelResponse, model: &str, id: &str, created: u64) -> Vec<u8> {
    let chunk = |delta: Value, finish: Value, usage: Value| {
        let chunk = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
            "usage": usage,
        });
        format!("data: {chunk}\n\n")
    };
    let mut stream = chunk(
        json!({"role": "assistant", "content": ""}),
        Value::Null,
        Value::Null,
    );
    if let Some(text) = text_of(response) {
        stream.push_str(&chunk(json!({"content": text}), Value::Null, Value::Null));
    }
    for (index, mut call) in calls_of(response).into_iter().enumerate() {
        call["index"] = json!(index);
        stream.push_str(&chunk(
            json!({"tool_calls": [call]}),
            Value::Null,
            Value::Null,
        ));
    }
    stream.push_str(&chunk(
        json!({}),
        json!(finish_reason(response.stop_reason)),
        usage(response),
    ));
    stream.push_str("data: [DONE]\n\n");
    stream.into_bytes()
}

/// The model's answer to `asked`, whole or as the stream it asked for.
#[must_use]
pub fn answer(response: &ModelResponse, asked: &Asked) -> Answer {
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let created = tilo_clock::unix_now();
    if asked.stream {
        Answer {
            status: 200,
            content_type: EVENTS,
            body: chunks(response, &asked.model, &id, created),
        }
    } else {
        Answer {
            status: 200,
            content_type: JSON,
            body: completion(response, &asked.model, &id, created)
                .to_string()
                .into_bytes(),
        }
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
