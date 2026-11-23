//! The model APIs a signed-in person asks: the `ChatGPT` backend, `OpenAI`'s API and
//! Anthropic's Messages API.
//!
//! Every one answers from the same script, so switching providers or models is visible in
//! the answer itself: a text answer starts `[<Provider> <model>]`, and a newest user
//! message saying `do <tool> <json-args>` (a line per call, for tools the request offers)
//! is answered with those calls. A request whose newest input is tool results is answered
//! with their summary. Each answer reports usage in the counters its API sends.

use serde_json::{Map, Value, json};

use crate::{ACCOUNT_ID, API_KEY_PREFIX, Answer, Recorded, State, bearer, live_access};

/// Which provider's API a request reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    ChatGpt,
    OpenAi,
    Anthropic,
}

impl Kind {
    fn provider(self) -> &'static str {
        match self {
            Self::ChatGpt => "ChatGPT",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }
}

/// The routes of one provider's API, matched by their last segment so an endpoint that
/// already names `/v1` (which the wire code then appends its own path to) is served too.
pub(crate) fn route(state: &State, request: &Recorded, kind: Kind, rest: &str) -> Answer {
    match (request.method.as_str(), rest) {
        ("GET", path) if path.ends_with("/models") => Answer::json(200, &model_list(kind)),
        ("POST", path) if path.ends_with("/responses") => {
            if let Some(denied) = denied(state, request, kind) {
                return denied;
            }
            let turn = responses_turn(&request.json, model(&request.json));
            let said = script(kind, &turn);
            let input_tokens = bytes4(request.bytes);
            if request.json.get("stream") == Some(&Value::Bool(true)) {
                Answer::sse(responses_stream(&turn, &said, input_tokens))
            } else {
                Answer::json(200, &responses_answer(&turn, &said, input_tokens))
            }
        }
        ("POST", path) if path.ends_with("/messages") => {
            if let Some(denied) = denied(state, request, kind) {
                return denied;
            }
            let turn = anthropic_turn(&request.json, model(&request.json));
            let said = script(kind, &turn);
            let input_tokens = bytes4(request.bytes);
            if request.json.get("stream") == Some(&Value::Bool(true)) {
                Answer::sse(anthropic_stream(&turn, &said, input_tokens))
            } else {
                Answer::json(200, &anthropic_answer(&turn, &said, input_tokens))
            }
        }
        ("POST", path) if path.ends_with("/chat/completions") => {
            if let Some(denied) = denied(state, request, kind) {
                return denied;
            }
            let turn = chat_turn(&request.json, model(&request.json));
            let said = script(kind, &turn);
            let input_tokens = bytes4(request.bytes);
            if request.json.get("stream") == Some(&Value::Bool(true)) {
                Answer::sse(chat_stream(&turn, &said, input_tokens))
            } else {
                Answer::json(200, &chat_answer(&turn, &said, input_tokens))
            }
        }
        _ => Answer::json(404, &json!({"error": "not found"})),
    }
}

/// The models each provider lists, in the shape Tilo's model-list reader parses.
fn model_list(kind: Kind) -> Value {
    match kind {
        Kind::ChatGpt => json!({"models": [
            {"slug": "gpt-5.5", "display_name": "GPT-5.5"},
            {"slug": "gpt-5.5-mini", "display_name": "GPT-5.5 Mini"},
        ]}),
        Kind::OpenAi => json!({"object": "list", "data": [
            {"id": "gpt-5.5", "object": "model", "owned_by": "fake"},
            {"id": "gpt-5.5-mini", "object": "model", "owned_by": "fake"},
        ]}),
        Kind::Anthropic => json!({"data": [
            {"type": "model", "id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5"},
            {"type": "model", "id": "claude-haiku-5-5", "display_name": "Claude Haiku 5.5"},
        ], "has_more": false}),
    }
}

/// The refusal a request earns when it carries no credential this provider accepts.
fn denied(state: &State, request: &Recorded, kind: Kind) -> Option<Answer> {
    match kind {
        Kind::ChatGpt => {
            let Some(token) = bearer(request) else {
                return Some(unauthorized("an access token is required"));
            };
            if !live_access(state, token) {
                return Some(unauthorized("the access token is unknown or has expired"));
            }
            (request.header("chatgpt-account-id") != Some(ACCOUNT_ID))
                .then(|| unauthorized("the request names no account"))
        }
        Kind::OpenAi => (!bearer(request).is_some_and(|token| token.starts_with(API_KEY_PREFIX)))
            .then(|| unauthorized("an API key is required")),
        Kind::Anthropic => request
            .header("x-api-key")
            .is_none_or(str::is_empty)
            .then(|| unauthorized("an x-api-key is required")),
    }
}

fn unauthorized(why: &str) -> Answer {
    Answer::json(
        401,
        &json!({"error": {"message": why, "type": "invalid_request_error", "code": "invalid_api_key"}}),
    )
}

/// What a request asks the model: its model, the tools it offers, its newest user text and
/// the tool results after it.
#[derive(Debug, Default)]
struct Turn {
    model: String,
    tools: Vec<String>,
    user: String,
    results: Vec<String>,
}

fn model(body: &Value) -> String {
    body.get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A Responses request: `input` items, and tools named at their top level.
fn responses_turn(body: &Value, model: String) -> Turn {
    let items = array(body, "input");
    let tools = array(body, "tools")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    let mut user = String::new();
    let mut newest_user = None;
    for (at, item) in items.iter().enumerate() {
        if item.get("role").and_then(Value::as_str) == Some("user")
            && item.get("type").and_then(Value::as_str) != Some("function_call_output")
        {
            newest_user = Some(at);
            user = item_text(item);
        }
    }
    let results = items
        .iter()
        .skip(newest_user.map_or(0, |at| at.saturating_add(1)))
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call_output"))
        .filter_map(|item| item.get("output").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    Turn {
        model,
        tools,
        user,
        results,
    }
}

/// An Anthropic request: `messages` with content blocks, and tools named at their top
/// level. The turn's tool results sit in the last user message, as blocks.
fn anthropic_turn(body: &Value, model: String) -> Turn {
    let messages = array(body, "messages");
    let tools = array(body, "tools")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    let mut user = String::new();
    for message in &messages {
        if message.get("role").and_then(Value::as_str) == Some("user") {
            let text = blocks_of(message, "text")
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                user = text;
            }
        }
    }
    let results = messages
        .last()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .map(|message| {
            blocks_of(message, "tool_result")
                .iter()
                .map(result_text)
                .collect()
        })
        .unwrap_or_default();
    Turn {
        model,
        tools,
        user,
        results,
    }
}

/// A chat completions request: `messages` with string content, `tool` messages for
/// results, and tools named under `function`.
fn chat_turn(body: &Value, model: String) -> Turn {
    let messages = array(body, "messages");
    let tools = array(body, "tools")
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    let mut user = String::new();
    let mut newest_user = None;
    for (at, message) in messages.iter().enumerate() {
        if message.get("role").and_then(Value::as_str) == Some("user") {
            newest_user = Some(at);
            user = item_text(message);
        }
    }
    let results = messages
        .iter()
        .skip(newest_user.map_or(0, |at| at.saturating_add(1)))
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        .filter_map(|message| message.get("content").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    Turn {
        model,
        tools,
        user,
        results,
    }
}

fn array(value: &Value, key: &str) -> Vec<Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn blocks_of(message: &Value, kind: &str) -> Vec<Value> {
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some(kind))
        .cloned()
        .collect()
}

/// The text a Responses input item carries, as a string or as text parts.
fn item_text(item: &Value) -> String {
    match item.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => item
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

/// The text of an Anthropic tool result, as a string or as text blocks.
fn result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// What the fake answers: text, or the calls the request asked it to make.
enum Said {
    Text(String),
    Calls(Vec<(String, Value)>),
}

/// The scripted answer, the same on every provider so a model switch shows in the text.
fn script(kind: Kind, turn: &Turn) -> Said {
    let prefix = format!("[{} {}]", kind.provider(), turn.model);
    if !turn.results.is_empty() {
        let each: Vec<String> = turn
            .results
            .iter()
            .map(|result| truncate(result.trim(), 200))
            .collect();
        return Said::Text(format!("{prefix} Done: {}", each.join("; ")));
    }
    let calls = do_calls(turn);
    if !calls.is_empty() {
        return Said::Calls(calls);
    }
    Said::Text(format!("{prefix} You said: {}", turn.user))
}

/// The `do <tool> <json-args>` lines of the newest user message whose tool the request
/// offers, each a call.
fn do_calls(turn: &Turn) -> Vec<(String, Value)> {
    turn.user
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("do ")?;
            let (name, arguments) = rest.split_once(' ')?;
            if !turn.tools.iter().any(|tool| tool == name) {
                return None;
            }
            let input: Value = serde_json::from_str(arguments).ok()?;
            input.is_object().then(|| (name.to_owned(), input))
        })
        .collect()
}

fn truncate(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

fn bytes4(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX) / 4
}

fn answer_bytes(said: &Said) -> u64 {
    match said {
        Said::Text(text) => bytes4(text.len()),
        Said::Calls(calls) => bytes4(calls.iter().map(|(_, input)| input.to_string().len()).sum()),
    }
}

fn usage(input_tokens: u64, output_tokens: u64) -> Value {
    json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "total_tokens": input_tokens.saturating_add(output_tokens),
    })
}

/// The output items of a Responses answer.
fn responses_output(said: &Said) -> Vec<Value> {
    match said {
        Said::Text(text) => vec![json!({
            "type": "message",
            "id": "msg_fake",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "annotations": [], "text": text}],
        })],
        Said::Calls(calls) => calls
            .iter()
            .enumerate()
            .map(|(at, (name, input))| {
                json!({
                    "type": "function_call",
                    "id": format!("fc_{at}"),
                    "call_id": format!("call_{at}"),
                    "name": name,
                    "arguments": input.to_string(),
                })
            })
            .collect(),
    }
}

fn responses_response(status: &str, turn: &Turn, output: &Value, counters: &Value) -> Value {
    json!({
        "id": "resp_fake",
        "object": "response",
        "created_at": 0,
        "status": status,
        "model": turn.model,
        "output": output,
        "tools": [],
        "usage": counters,
    })
}

/// A whole Responses answer, for a client that asked for no stream.
fn responses_answer(turn: &Turn, said: &Said, input_tokens: u64) -> Value {
    let output = Value::Array(responses_output(said));
    responses_response(
        "completed",
        turn,
        &output,
        &usage(input_tokens, answer_bytes(said)),
    )
}

/// A streamed Responses answer: the events Tilo's Responses reader folds back into one.
fn responses_stream(turn: &Turn, said: &Said, input_tokens: u64) -> String {
    let mut out = String::new();
    let opening = json!({
        "type": "response.created",
        "response": responses_response(
            "in_progress",
            turn,
            &json!([]),
            &usage(input_tokens, 0),
        ),
    });
    event(&mut out, "response.created", &opening);
    match said {
        Said::Text(text) => {
            let added = json!({
                "type": "message", "id": "msg_fake", "role": "assistant",
                "status": "in_progress", "content": [],
            });
            event(
                &mut out,
                "response.output_item.added",
                &json!({
                    "type": "response.output_item.added", "output_index": 0, "item": added,
                }),
            );
            event(
                &mut out,
                "response.output_text.delta",
                &json!({
                    "type": "response.output_text.delta", "item_id": "msg_fake",
                    "output_index": 0, "content_index": 0, "delta": text,
                }),
            );
            let done = json!({
                "type": "message", "id": "msg_fake", "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "annotations": [], "text": text}],
            });
            event(
                &mut out,
                "response.output_item.done",
                &json!({
                    "type": "response.output_item.done", "output_index": 0, "item": done,
                }),
            );
        }
        Said::Calls(calls) => {
            for (at, (name, input)) in calls.iter().enumerate() {
                let arguments = input.to_string();
                let opened = json!({
                    "type": "function_call", "id": format!("fc_{at}"),
                    "call_id": format!("call_{at}"), "name": name, "arguments": "",
                });
                event(
                    &mut out,
                    "response.output_item.added",
                    &json!({
                        "type": "response.output_item.added", "output_index": at, "item": opened,
                    }),
                );
                event(
                    &mut out,
                    "response.function_call_arguments.delta",
                    &json!({
                        "type": "response.function_call_arguments.delta",
                        "output_index": at, "delta": arguments,
                    }),
                );
                let done = json!({
                    "type": "function_call", "id": format!("fc_{at}"),
                    "call_id": format!("call_{at}"), "name": name, "arguments": arguments,
                });
                event(
                    &mut out,
                    "response.output_item.done",
                    &json!({
                        "type": "response.output_item.done", "output_index": at, "item": done,
                    }),
                );
            }
        }
    }
    let output = Value::Array(responses_output(said));
    event(
        &mut out,
        "response.completed",
        &json!({
            "type": "response.completed",
            "response": responses_response(
                "completed",
                turn,
                &output,
                &usage(input_tokens, answer_bytes(said)),
            ),
        }),
    );
    out
}

/// The content blocks of a whole Anthropic message.
fn anthropic_blocks(said: &Said) -> Vec<Value> {
    match said {
        Said::Text(text) => vec![json!({"type": "text", "text": text})],
        Said::Calls(calls) => calls
            .iter()
            .enumerate()
            .map(|(at, (name, input))| {
                json!({
                    "type": "tool_use", "id": format!("toolu_{at}"),
                    "name": name, "input": input,
                })
            })
            .collect(),
    }
}

fn anthropic_stop(said: &Said) -> &'static str {
    match said {
        Said::Text(_) => "end_turn",
        Said::Calls(_) => "tool_use",
    }
}

/// A whole Messages answer, for a client that asked for no stream.
fn anthropic_answer(turn: &Turn, said: &Said, input_tokens: u64) -> Value {
    json!({
        "id": "msg_fake",
        "type": "message",
        "role": "assistant",
        "model": turn.model,
        "content": anthropic_blocks(said),
        "stop_reason": anthropic_stop(said),
        "stop_sequence": null,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": answer_bytes(said),
        },
    })
}

/// A streamed Messages answer: the events Tilo's Messages reader folds back into one.
fn anthropic_stream(turn: &Turn, said: &Said, input_tokens: u64) -> String {
    let mut out = String::new();
    event(
        &mut out,
        "message_start",
        &json!({
            "type": "message_start",
            "message": {
                "id": "msg_fake", "type": "message", "role": "assistant",
                "model": turn.model, "content": [], "stop_reason": null,
                "usage": {"input_tokens": input_tokens, "output_tokens": 1},
            },
        }),
    );
    match said {
        Said::Text(text) => {
            event(
                &mut out,
                "content_block_start",
                &json!({
                    "type": "content_block_start", "index": 0,
                    "content_block": {"type": "text", "text": ""},
                }),
            );
            event(
                &mut out,
                "content_block_delta",
                &json!({
                    "type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": text},
                }),
            );
            event(
                &mut out,
                "content_block_stop",
                &json!({
                    "type": "content_block_stop", "index": 0,
                }),
            );
        }
        Said::Calls(calls) => {
            for (at, (name, input)) in calls.iter().enumerate() {
                event(
                    &mut out,
                    "content_block_start",
                    &json!({
                        "type": "content_block_start", "index": at,
                        "content_block": {
                            "type": "tool_use", "id": format!("toolu_{at}"), "name": name,
                            "input": {},
                        },
                    }),
                );
                event(
                    &mut out,
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta", "index": at,
                        "delta": {"type": "input_json_delta", "partial_json": input.to_string()},
                    }),
                );
                event(
                    &mut out,
                    "content_block_stop",
                    &json!({
                        "type": "content_block_stop", "index": at,
                    }),
                );
            }
        }
    }
    event(
        &mut out,
        "message_delta",
        &json!({
            "type": "message_delta",
            "delta": {"stop_reason": anthropic_stop(said)},
            "usage": {"output_tokens": answer_bytes(said)},
        }),
    );
    event(&mut out, "message_stop", &json!({"type": "message_stop"}));
    out
}

/// The whole answer of a chat completions client that asked for no stream.
fn chat_answer(turn: &Turn, said: &Said, input_tokens: u64) -> Value {
    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    match said {
        Said::Text(text) => {
            message.insert("content".into(), json!(text));
        }
        Said::Calls(calls) => {
            message.insert("content".into(), Value::Null);
            let calls: Vec<Value> = calls
                .iter()
                .enumerate()
                .map(|(at, (name, input))| {
                    json!({
                        "index": at, "id": format!("call_{at}"), "type": "function",
                        "function": {"name": name, "arguments": input.to_string()},
                    })
                })
                .collect();
            message.insert("tool_calls".into(), Value::Array(calls));
        }
    }
    json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion",
        "created": 0,
        "model": turn.model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": match said {
                Said::Text(_) => "stop",
                Said::Calls(_) => "tool_calls",
            },
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": answer_bytes(said),
            "total_tokens": input_tokens.saturating_add(answer_bytes(said)),
        },
    })
}

/// A streamed chat answer: the chunks Tilo's chat reader folds back into one.
fn chat_stream(turn: &Turn, said: &Said, input_tokens: u64) -> String {
    let mut out = String::new();
    let chunk = |delta: Value, finish: Value| {
        json!({
            "id": "chatcmpl-fake",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": turn.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    };
    let opening = chunk(json!({"role": "assistant", "content": ""}), Value::Null);
    data(&mut out, &opening);
    match said {
        Said::Text(text) => {
            data(&mut out, &chunk(json!({"content": text}), Value::Null));
        }
        Said::Calls(calls) => {
            for (at, (name, input)) in calls.iter().enumerate() {
                data(
                    &mut out,
                    &chunk(
                        json!({"tool_calls": [{
                            "index": at,
                            "id": format!("call_{at}"),
                            "type": "function",
                            "function": {"name": name, "arguments": input.to_string()},
                        }]}),
                        Value::Null,
                    ),
                );
            }
        }
    }
    let finish = match said {
        Said::Text(_) => "stop",
        Said::Calls(_) => "tool_calls",
    };
    data(&mut out, &chunk(json!({}), json!(finish)));
    let output = answer_bytes(said);
    data(
        &mut out,
        &json!({
            "id": "chatcmpl-fake",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": turn.model,
            "choices": [],
            "usage": {
                "prompt_tokens": input_tokens,
                "completion_tokens": output,
                "total_tokens": input_tokens.saturating_add(output),
            },
        }),
    );
    out.push_str("data: [DONE]\n\n");
    out
}

/// One server-sent event, as Responses and Messages streams carry it.
fn event(out: &mut String, name: &str, data: &Value) {
    out.push_str("event: ");
    out.push_str(name);
    out.push_str("\ndata: ");
    out.push_str(&data.to_string());
    out.push_str("\n\n");
}

/// One data-only server-sent event, as chat streams carry it.
fn data(out: &mut String, chunk: &Value) {
    out.push_str("data: ");
    out.push_str(&chunk.to_string());
    out.push_str("\n\n");
}
