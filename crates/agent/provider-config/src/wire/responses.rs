//! `OpenAI` Responses, stateless: the request, a whole answer, and the
//! events of a streamed one, whose last event carries the whole answer.
//!
//! Nothing is kept by the provider (`store: false`), so a turn that reasoned
//! is carried into the next as the encrypted reasoning item it answered
//! with. Each reasoning block and each call keeps its item's id (and the
//! reasoning its ciphertext) as an opaque extension; a request rebuilds the
//! items from those checked fields only, so an extension can name an id and
//! a ciphertext, never another kind of item.

use serde_json::{Map, Value, json};

use super::{Api, Delta, count, extra, put};
use crate::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall,
    ToolChoice, Usage,
};

/// The extension key of a block's Responses item.
const EXTRA: &str = "openai_responses";
/// The most ciphertext one replayed reasoning item may carry.
const MOST_CIPHERTEXT: usize = 1 << 20;
/// What the `ChatGPT` backend is told when the conversation gives it no
/// instructions: it refuses a request without them.
const DEFAULT_INSTRUCTIONS: &str = "You are a helpful assistant.";
/// The `include` entry asking for a turn's reasoning as ciphertext.
const CIPHERTEXT: &str = "reasoning.encrypted_content";

pub(super) fn body(request: &ModelRequest, api: Api) -> Result<Map<String, Value>, ModelError> {
    let mut instructions: Vec<&str> = Vec::new();
    let mut input = Vec::new();
    let mut first_system = true;
    for message in &request.messages {
        let text = || {
            message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        match message.role {
            // `OpenAI`'s API takes the first system message as its
            // instructions and the rest as system turns, so a change to the
            // context after them leaves the instructions and tools in the
            // prompt cache. The `ChatGPT` backend takes all of them as its
            // instructions.
            Role::System if first_system || api == Api::ChatGpt => {
                first_system = false;
                for content in &message.content {
                    if let Content::Text { text } = content {
                        instructions.push(text);
                    }
                }
            }
            Role::System => input.push(json!({"role": "system", "content": text()})),
            Role::User => {
                let parts: Vec<Value> = message
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        Content::Text { text } => Some(json!({"type": "input_text", "text": text})),
                        Content::Image { media_type, base64 } => Some(json!({
                            "type": "input_image",
                            "image_url": format!("data:{media_type};base64,{base64}"),
                        })),
                        _ => None,
                    })
                    .collect();
                if !parts.is_empty() {
                    input.push(json!({"role": "user", "content": parts}));
                }
            }
            Role::Assistant => {
                for content in &message.content {
                    if let Some(item) = assistant_item(content)? {
                        input.push(item);
                    }
                }
            }
            Role::Tool => {
                for content in &message.content {
                    match content {
                        Content::ToolResult(result) => input.push(json!({
                            "type": "function_call_output",
                            "call_id": result.call_id,
                            "output": result.content,
                        })),
                        Content::Text { text } => input.push(json!({
                            "role": "user",
                            "content": [{"type": "input_text", "text": text}],
                        })),
                        _ => {}
                    }
                }
            }
        }
    }
    let mut object = Map::new();
    let instructions = instructions.join("\n\n");
    if !instructions.is_empty() {
        object.insert("instructions".into(), instructions.into());
    } else if api == Api::ChatGpt {
        object.insert("instructions".into(), DEFAULT_INSTRUCTIONS.into());
    }
    object.insert("input".into(), Value::Array(input));
    if !request.tools.is_empty() && request.tool_choice != ToolChoice::None {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                    "strict": false,
                })
            })
            .collect();
        object.insert("tools".into(), Value::Array(tools));
        object.insert(
            "tool_choice".into(),
            match request.tool_choice {
                ToolChoice::Required => "required",
                _ => "auto",
            }
            .into(),
        );
    }
    Ok(object)
}

/// The input item an assistant block replays as, if any.
fn assistant_item(content: &Content) -> Result<Option<Value>, ModelError> {
    Ok(match content {
        Content::Text { text } if !text.is_empty() => {
            Some(json!({"role": "assistant", "content": text}))
        }
        Content::ToolCall(call) => {
            let mut item = json!({
                "type": "function_call",
                "call_id": call.id,
                "name": call.name,
                "arguments": serde_json::to_string(&call.input)
                    .map_err(|_| ModelError::invalid("unwritable tool arguments"))?,
            });
            if let Some(id) = item_id(extra(call.extra_content.as_ref(), EXTRA), "fc_") {
                put(&mut item, "id", id.into());
            }
            Some(item)
        }
        Content::Reasoning {
            text,
            extra_content,
        } => {
            let kept = extra(extra_content.as_ref(), EXTRA);
            item_id(kept, "rs_")
                .zip(ciphertext(kept))
                .map(|(id, ciphertext)| {
                    let summary: Vec<Value> = (!text.is_empty())
                        .then(|| json!({"type": "summary_text", "text": text}))
                        .into_iter()
                        .collect();
                    json!({
                        "type": "reasoning",
                        "id": id,
                        "summary": summary,
                        "encrypted_content": ciphertext,
                    })
                })
        }
        _ => None,
    })
}

/// A reasoning turn's ciphertext is asked for, so it can be carried into
/// the next turn; a model that does not reason refuses it, so only then.
pub(super) fn ask_for_ciphertext(object: &mut Map<String, Value>) {
    let reasons = object
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("effort"))
        .is_some_and(|effort| effort != "none");
    let mut include: Vec<Value> = object
        .remove("include")
        .and_then(|include| include.as_array().cloned())
        .unwrap_or_default();
    include.retain(|item| item != CIPHERTEXT);
    if reasons {
        include.push(CIPHERTEXT.into());
    }
    if !include.is_empty() {
        object.insert("include".into(), Value::Array(include));
    }
}

/// The item id under `kept` when it is a Responses id with `prefix`.
fn item_id<'a>(kept: Option<&'a Value>, prefix: &str) -> Option<&'a str> {
    kept.and_then(|kept| kept.get("id"))
        .and_then(Value::as_str)
        .filter(|id| {
            id.len() <= 64
                && id.starts_with(prefix)
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

/// The reasoning ciphertext under `kept`.
fn ciphertext(kept: Option<&Value>) -> Option<&str> {
    kept.and_then(|kept| kept.get("encrypted_content"))
        .and_then(Value::as_str)
        .filter(|text| {
            !text.is_empty()
                && text.len() <= MOST_CIPHERTEXT
                && text.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'-' | b'_')
                })
        })
}

#[expect(
    clippy::too_many_lines,
    reason = "each output item's kind in turn, then stop and usage"
)]
pub(super) fn response(value: &Value) -> Result<ModelResponse, ModelError> {
    let invalid = |why: &str| ModelError::invalid(format!("the provider's answer {why}"));
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("had no output"))?;
    let mut content = Vec::new();
    let mut refused = false;
    for item in output {
        let text = |key: &str| item.get(key).and_then(Value::as_str);
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => content.push(Content::Text {
                            text: part
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .into(),
                        }),
                        Some("refusal") => refused = true,
                        _ => {}
                    }
                }
            }
            Some("reasoning") => {
                let said: String = item
                    .get("summary")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let kept = item_id(Some(item), "rs_")
                    .zip(ciphertext(Some(item)))
                    .map(|(id, ciphertext)| {
                        json!({ EXTRA: {"id": id, "encrypted_content": ciphertext} })
                    });
                if !said.is_empty() || kept.is_some() {
                    content.push(Content::Reasoning {
                        text: said,
                        extra_content: kept,
                    });
                }
            }
            Some("function_call") => {
                let (Some(id), Some(name), Some(arguments)) =
                    (text("call_id"), text("name"), text("arguments"))
                else {
                    return Err(invalid("had a tool call without its id, name or arguments"));
                };
                let input: Value = if arguments.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(arguments)
                        .map_err(|_| invalid("had tool arguments that are not JSON"))?
                };
                let call = ToolCall {
                    id: id.into(),
                    name: name.into(),
                    input,
                    extra_content: item_id(Some(item), "fc_")
                        .map(|item| json!({ EXTRA: {"id": item} })),
                };
                call.validate()?;
                content.push(Content::ToolCall(call));
            }
            _ => {}
        }
    }
    let calls = content
        .iter()
        .any(|block| matches!(block, Content::ToolCall(_)));
    let stop_reason = match value.get("status").and_then(Value::as_str) {
        _ if refused => StopReason::Refusal,
        Some("completed") | None if calls => StopReason::ToolCalls,
        Some("completed") | None => StopReason::EndTurn,
        Some("incomplete") => match value
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
        {
            Some("content_filter") => StopReason::Refusal,
            _ => StopReason::OutputLimit,
        },
        Some(_) => return Err(invalid("did not complete")),
    };
    let usage = value.get("usage");
    let input_tokens = count(usage, "/input_tokens");
    let output_tokens = count(usage, "/output_tokens");
    Ok(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
        },
        stop_reason,
        cost_micros: 0,
        usage: Usage {
            input_tokens,
            output_tokens,
            total_tokens: count(usage, "/total_tokens")
                .max(input_tokens.saturating_add(output_tokens)),
            cached_input_tokens: count(usage, "/input_tokens_details/cached_tokens"),
            cache_write_tokens: 0,
            reasoning_tokens: count(usage, "/output_tokens_details/reasoning_tokens"),
        },
    })
}

/// A streamed Responses answer: deltas as they come, and the whole answer
/// its final event carries.
#[derive(Default)]
pub(super) struct Fold {
    /// The output index of each tool call, in the order they opened.
    calls: Vec<u64>,
    answer: Option<Value>,
}

impl Fold {
    pub(super) fn event(&mut self, event: &Value, out: &mut Vec<Delta>) -> Result<(), String> {
        if self.answer.is_some() {
            return Err("the provider wrote after its answer ended".into());
        }
        let text = |key: &str| event.get(key).and_then(Value::as_str);
        let call = |calls: &[u64]| {
            event
                .get("output_index")
                .and_then(Value::as_u64)
                .and_then(|at| calls.iter().position(|call| *call == at))
        };
        match text("type").unwrap_or_default() {
            "error" | "response.failed" => return Err(event.to_string()),
            "response.output_text.delta" => {
                out.push(Delta::Text(text("delta").unwrap_or_default().into()));
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                out.push(Delta::Reasoning(text("delta").unwrap_or_default().into()));
            }
            "response.output_item.added" => {
                let item = event.get("item");
                if item
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    == Some("function_call")
                {
                    if self.calls.len() >= super::sse::MOST_CALLS {
                        return Err("too many tool calls".into());
                    }
                    let field = |key: &str| {
                        item.and_then(|item| item.get(key))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    };
                    out.push(Delta::CallOpened {
                        index: self.calls.len(),
                        id: field("call_id"),
                        name: field("name").unwrap_or_default(),
                    });
                    self.calls.push(
                        event
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .unwrap_or(u64::MAX),
                    );
                }
            }
            "response.function_call_arguments.delta" => {
                let index = call(&self.calls).ok_or("arguments for a call never opened")?;
                out.push(Delta::CallArguments {
                    index,
                    json: text("delta").unwrap_or_default().into(),
                });
            }
            "response.output_item.done" => {
                let item = event.get("item");
                if item
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    == Some("function_call")
                {
                    let index = call(&self.calls).ok_or("the end of a call never opened")?;
                    let field = |key: &str| {
                        item.and_then(|item| item.get(key))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned()
                    };
                    out.push(Delta::CallClosed {
                        index,
                        id: field("call_id"),
                        name: field("name"),
                    });
                }
            }
            "response.completed" | "response.incomplete" => {
                self.answer = Some(
                    event
                        .get("response")
                        .filter(|response| response.is_object())
                        .ok_or("the provider's last event carried no answer")?
                        .clone(),
                );
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn finished(&self) -> bool {
        self.answer.is_some()
    }

    pub(super) fn answer(self) -> Result<Value, String> {
        self.answer
            .ok_or_else(|| "the provider sent no answer".into())
    }
}
