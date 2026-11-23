//! A coding harness's model endpoint, in its provider's own words.
//!
//! A harness asks a model the way its provider is asked: Claude Code posts to
//! `/v1/messages` and reads Anthropic's Messages answers. That address is its
//! run's own endpoint ([`super::endpoint`]), and what it asks is answered by the
//! model the workspace chose for the sub-agent's kind of work, whichever provider
//! that is. This module is the two translations and
//! nothing else: a request in the provider's shape into the host's own
//! [`ModelRequest`], and the host's [`ModelResponse`] or refusal back into the
//! provider's shape, whole or as the provider's stream of events.
//!
//! What does not cross: a provider's own tools (only the provider runs them),
//! reasoning (the host shows none of it to a harness, so none comes back to
//! be signed), and the model's name, which the harness is told back as it
//! said it: the person's choice answers, not the harness's.

use serde_json::{Map, Value, json};

use crate::workspaces::person_model::protocol::{
    Content, ErrorKind, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason,
    ToolCall, ToolChoice, ToolDefinition, ToolResult,
};

/// The most of a request's body the host reads: a whole conversation with
/// its pictures.
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;

/// What an answer may take when the harness names no limit.
const DEFAULT_OUTPUT_TOKENS: u32 = 4096;

/// What a harness sent beside words that this endpoint does not pass on.
const NOT_PASSED: &str = "[something was attached here that this model endpoint does not pass on]";

/// A request a harness made of its model.
#[derive(Clone, Debug, PartialEq)]
pub struct Asked {
    pub request: ModelRequest,
    /// The model the harness named: said back to it, never followed.
    pub model: String,
    /// Whether it reads its answer as a stream of events.
    pub stream: bool,
}

/// What a request at the endpoint is.
#[derive(Clone, Debug, PartialEq)]
pub enum Wanted {
    /// An answer of the model.
    Message(Box<Asked>),
    /// How many tokens a request would be, about.
    Count(u64),
}

/// An answer in the provider's shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

const JSON: &str = "application/json";
const EVENTS: &str = "text/event-stream";

/// A failure in the provider's shape: `kind` is the provider's own word for
/// it, which decides whether the harness asks again.
#[must_use]
pub fn failure(status: u16, kind: &str, words: &str) -> Answer {
    Answer {
        status,
        content_type: JSON,
        body: json!({"type": "error", "error": {"type": kind, "message": words}})
            .to_string()
            .into_bytes(),
    }
}

/// Why the workspace's model did not answer, as the provider would say it: the status, the
/// provider's word, and the reason in words a person can act on. Only what may pass by itself is
/// said in a way a harness asks again for; a limit, a missing key or a missing choice ends its
/// turn.
#[must_use]
pub fn refused(error: &ModelError) -> (u16, &'static str, String) {
    let (status, kind) = match error.kind {
        ErrorKind::Authentication => (401, "authentication_error"),
        ErrorKind::PermissionDenied | ErrorKind::Refused => (403, "permission_error"),
        ErrorKind::CreditsExhausted => (402, "billing_error"),
        ErrorKind::NoProvider => (404, "not_found_error"),
        ErrorKind::InvalidRequest | ErrorKind::ContextLimit | ErrorKind::Unsupported => {
            (400, "invalid_request_error")
        }
        ErrorKind::RateLimited => (429, "rate_limit_error"),
        ErrorKind::Unavailable | ErrorKind::Transport | ErrorKind::TimedOut => {
            (529, "overloaded_error")
        }
        _ => (500, "api_error"),
    };
    (status, kind, error.message.clone())
}

/// What the request `method` `path` with `body` asks of the endpoint, or the
/// answer that says why it is nothing this endpoint does.
pub fn wanted(method: &str, path: &str, body: &[u8]) -> Result<Wanted, Answer> {
    let route = path.split('?').next().unwrap_or_default();
    match (method, route.trim_end_matches('/')) {
        ("POST", "/v1/messages") => parse(body)
            .map(|asked| Wanted::Message(Box::new(asked)))
            .map_err(|why| failure(400, "invalid_request_error", &why)),
        // About four bytes a token: the harness only sizes its context by it.
        ("POST", "/v1/messages/count_tokens") => Ok(Wanted::Count(
            u64::try_from(body.len() / 4).unwrap_or(u64::MAX),
        )),
        _ => Err(failure(
            404,
            "not_found_error",
            "this endpoint answers model messages and nothing else",
        )),
    }
}

/// The answer to a count of tokens.
#[must_use]
pub fn counted(tokens: u64) -> Answer {
    Answer {
        status: 200,
        content_type: JSON,
        body: json!({"input_tokens": tokens}).to_string().into_bytes(),
    }
}

fn parse(body: &[u8]) -> Result<Asked, String> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| format!("the request is not JSON: {error}"))?;
    let object = value.as_object().ok_or("the request is not an object")?;
    let mut messages = Vec::new();
    let system = object.get("system").map(words).unwrap_or_default();
    if !system.is_empty() {
        messages.push(Message::text(Role::System, system));
    }
    let turns = object
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("the request has no messages")?;
    for turn in turns {
        said(turn, &mut messages)?;
    }
    let tools: Vec<ToolDefinition> = object
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| tools.iter().filter_map(tool).collect())
        .unwrap_or_default();
    let chosen = object
        .get("tool_choice")
        .and_then(|choice| choice.get("type"))
        .and_then(Value::as_str);
    let tool_choice = match chosen {
        _ if tools.is_empty() => ToolChoice::Auto,
        Some("any" | "tool") => ToolChoice::Required,
        Some("none") => ToolChoice::None,
        _ => ToolChoice::Auto,
    };
    let max_output_tokens = object
        .get("max_tokens")
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

/// The words of a value that is text, or a list of blocks of which the text
/// ones are taken.
fn words(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// A picture block the harness sent inline; one it only names by an address
/// is not fetched, and said so.
fn picture(block: &Map<String, Value>) -> Content {
    let source = block.get("source").and_then(Value::as_object);
    let inline =
        source.filter(|source| source.get("type").and_then(Value::as_str) == Some("base64"));
    match inline.and_then(|source| {
        Some((
            source.get("media_type")?.as_str()?,
            source.get("data")?.as_str()?,
        ))
    }) {
        Some((media_type, data)) => Content::Image {
            media_type: media_type.to_owned(),
            base64: data.to_owned(),
        },
        None => Content::Text {
            text: NOT_PASSED.to_owned(),
        },
    }
}

/// A tool's result as the harness sent it: its words, and the pictures it
/// carried, which follow it as a message of their own.
fn result(block: &Map<String, Value>, pictures: &mut Vec<Content>) -> ToolResult {
    let mut content = String::new();
    match block.get("content") {
        Some(Value::String(text)) => content.push_str(text),
        Some(Value::Array(parts)) => {
            for part in parts.iter().filter_map(Value::as_object) {
                let piece = match part.get("type").and_then(Value::as_str) {
                    Some("text") => part.get("text").and_then(Value::as_str).unwrap_or_default(),
                    Some("image") => {
                        pictures.push(picture(part));
                        "[a picture, which follows]"
                    }
                    _ => NOT_PASSED,
                };
                if !content.is_empty() && !piece.is_empty() {
                    content.push('\n');
                }
                content.push_str(piece);
            }
        }
        _ => {}
    }
    ToolResult {
        call_id: block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        content,
        is_error: block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// One turn of the harness's conversation, added to `messages` as the host
/// writes one: a tool's results are a message of their own before what the
/// person's side says beside them.
fn said(turn: &Value, messages: &mut Vec<Message>) -> Result<(), String> {
    let role = match turn.get("role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        other => {
            return Err(format!(
                "a message's role is user or assistant, not {other:?}"
            ));
        }
    };
    let blocks = match turn.get("content") {
        Some(Value::String(text)) => {
            if !text.is_empty() {
                messages.push(Message::text(role, text.clone()));
            }
            return Ok(());
        }
        Some(Value::Array(blocks)) => blocks,
        _ => return Err("a message's content is text or a list of blocks".into()),
    };
    let (mut results, mut content, mut pictures) = (Vec::new(), Vec::new(), Vec::new());
    for block in blocks.iter().filter_map(Value::as_object) {
        let text = |name: &str| block.get(name).and_then(Value::as_str).unwrap_or_default();
        match (role, text("type")) {
            (_, "text") => {
                if !text("text").is_empty() {
                    content.push(Content::Text {
                        text: text("text").to_owned(),
                    });
                }
            }
            (Role::User, "image") => content.push(picture(block)),
            (Role::User, "tool_result") => {
                results.push(Content::ToolResult(result(block, &mut pictures)));
            }
            (Role::Assistant, "tool_use") => content.push(Content::ToolCall(ToolCall {
                id: text("id").to_owned(),
                name: text("name").to_owned(),
                input: block.get("input").cloned().unwrap_or_else(|| json!({})),
                extra_content: None,
            })),
            // Reasoning the harness kept is not sent again: the host shows
            // it none, so it has none of this conversation's to send.
            (Role::Assistant, "thinking" | "redacted_thinking") => {}
            _ => content.push(Content::Text {
                text: NOT_PASSED.to_owned(),
            }),
        }
    }
    if !results.is_empty() {
        messages.push(Message {
            role: Role::Tool,
            content: results,
        });
    }
    content.extend(pictures);
    if !content.is_empty() {
        messages.push(Message { role, content });
    }
    Ok(())
}

/// A tool the harness runs itself. One that only its provider runs (it has
/// a type of the provider's own) is not offered to another model.
fn tool(tool: &Value) -> Option<ToolDefinition> {
    let own = tool
        .get("type")
        .and_then(Value::as_str)
        .is_none_or(|kind| kind == "custom");
    own.then(|| {
        Some(ToolDefinition {
            name: tool.get("name")?.as_str()?.to_owned(),
            description: tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            input_schema: tool
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"})),
        })
    })
    .flatten()
}

/// The model's answer as the provider's message `id`, said to be of `model`.
#[must_use]
pub fn message(response: &ModelResponse, model: &str, id: &str) -> Value {
    let content: Vec<Value> = response
        .message
        .content
        .iter()
        .filter_map(|part| match part {
            Content::Text { text } if !text.is_empty() => {
                Some(json!({"type": "text", "text": text}))
            }
            Content::ToolCall(call) => Some(json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": call.input,
            })),
            _ => None,
        })
        .collect();
    let stop_reason = match response.stop_reason {
        StopReason::EndTurn => "end_turn",
        StopReason::ToolCalls => "tool_use",
        StopReason::OutputLimit => "max_tokens",
        StopReason::Refusal => "refusal",
    };
    let usage = response.usage;
    // The provider counts what was read from its cache and written to it
    // beside the input; the host counts them within it.
    let fresh = usage
        .input_tokens
        .saturating_sub(usage.cached_input_tokens)
        .saturating_sub(usage.cache_write_tokens);
    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": fresh,
            "cache_read_input_tokens": usage.cached_input_tokens,
            "cache_creation_input_tokens": usage.cache_write_tokens,
            "output_tokens": usage.output_tokens,
        },
    })
}

/// A whole answer, for a harness that did not ask for a stream.
#[must_use]
pub fn whole(message: &Value) -> Answer {
    Answer {
        status: 200,
        content_type: JSON,
        body: message.to_string().into_bytes(),
    }
}

/// The head of an answer that is a stream of events.
#[must_use]
pub const fn stream_begins() -> (u16, &'static str) {
    (200, EVENTS)
}

fn event(name: &str, data: &Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

/// The provider's stream of events that says `message`, a whole answer, from
/// its start to its stop: each block opened, said in one piece and closed.
#[must_use]
pub fn events(message: &Value) -> Vec<u8> {
    let mut started = message.clone();
    if let Some(started) = started.as_object_mut() {
        started.insert("content".into(), json!([]));
        started.insert("stop_reason".into(), Value::Null);
        if let Some(usage) = started.get_mut("usage").and_then(Value::as_object_mut) {
            usage.insert("output_tokens".into(), json!(0));
        }
    }
    let mut stream = event(
        "message_start",
        &json!({"type": "message_start", "message": started}),
    );
    let blocks = message.get("content").and_then(Value::as_array);
    for (index, block) in blocks.into_iter().flatten().enumerate() {
        let (opened, delta) = if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            let mut opened = block.clone();
            if let Some(opened) = opened.as_object_mut() {
                opened.insert("input".into(), json!({}));
            }
            let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
            (
                opened,
                json!({"type": "input_json_delta", "partial_json": input.to_string()}),
            )
        } else {
            (
                json!({"type": "text", "text": ""}),
                json!({"type": "text_delta", "text": block.get("text").cloned().unwrap_or_default()}),
            )
        };
        stream.push_str(&event(
            "content_block_start",
            &json!({"type": "content_block_start", "index": index, "content_block": opened}),
        ));
        stream.push_str(&event(
            "content_block_delta",
            &json!({"type": "content_block_delta", "index": index, "delta": delta}),
        ));
        stream.push_str(&event(
            "content_block_stop",
            &json!({"type": "content_block_stop", "index": index}),
        ));
    }
    stream.push_str(&event(
        "message_delta",
        &json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": message.get("stop_reason").cloned().unwrap_or_default(),
                "stop_sequence": null,
            },
            "usage": {
                "output_tokens": message.pointer("/usage/output_tokens").cloned().unwrap_or_default(),
            },
        }),
    ));
    stream.push_str(&event("message_stop", &json!({"type": "message_stop"})));
    stream.into_bytes()
}

#[cfg(test)]
#[path = "model_endpoint_tests.rs"]
mod tests;
