//! Anthropic Messages: the request, its prompt-cache breakpoints, a whole
//! answer, and the events of a streamed one folded back into a whole answer.

use serde_json::{Map, Value, json};

use super::{Delta, count, extra, put};
use crate::ProviderProfile;
use crate::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall,
    ToolChoice, Usage,
};

/// The extension key a thinking block keeps what replays it under: its
/// `signature`, or the `data` of a redacted one.
const EXTRA: &str = "anthropic";

pub(super) fn body(request: &ModelRequest, model: &str) -> Map<String, Value> {
    let mut system: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for message in &request.messages {
        if message.role == Role::System {
            let text: Vec<&str> = message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            system.push(text.join("\n"));
            continue;
        }
        let role = if message.role == Role::Assistant {
            "assistant"
        } else {
            "user"
        };
        let blocks: Vec<Value> = message.content.iter().filter_map(block).collect();
        if blocks.is_empty() {
            continue;
        }
        // The Messages API alternates roles: a turn of the same role as the
        // one before continues it.
        match messages.last_mut() {
            Some(last) if last.get("role").and_then(Value::as_str) == Some(role) => {
                if let Some(content) = last.get_mut("content").and_then(Value::as_array_mut) {
                    content.extend(blocks);
                }
            }
            _ => messages.push(json!({"role": role, "content": blocks})),
        }
    }
    let mut object = Map::new();
    // One system block per system message: stable instructions stay a
    // block of their own, so the context after them can change and leave
    // the instructions in the prompt cache.
    match system.as_slice() {
        [] => {}
        [only] => {
            object.insert("system".into(), only.clone().into());
        }
        several => {
            let blocks = several
                .iter()
                .filter(|text| !text.is_empty())
                .map(|text| json!({"type": "text", "text": text}))
                .collect();
            object.insert("system".into(), Value::Array(blocks));
        }
    }
    object.insert("messages".into(), Value::Array(messages));
    if !request.tools.is_empty() && request.tool_choice != ToolChoice::None {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            })
            .collect();
        object.insert("tools".into(), Value::Array(tools));
        // Told it must call a tool, Anthropic's newest models answer with an
        // error: they are left to choose, and a turn that calls none is its
        // caller's to ask again, as it is for every model.
        let forced = request.tool_choice == ToolChoice::Required && !refuses_forced_tools(model);
        object.insert(
            "tool_choice".into(),
            json!({"type": if forced { "any" } else { "auto" }}),
        );
    }
    object
}

fn block(content: &Content) -> Option<Value> {
    Some(match content {
        Content::Text { text } if !text.is_empty() => json!({"type": "text", "text": text}),
        Content::Text { .. } => return None,
        Content::Image { media_type, base64 } => json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": base64},
        }),
        Content::ToolCall(call) => json!({
            "type": "tool_use", "id": call.id, "name": call.name, "input": call.input,
        }),
        Content::ToolResult(result) => json!({
            "type": "tool_result",
            "tool_use_id": result.call_id,
            "content": result.content,
            "is_error": result.is_error,
        }),
        // Thinking goes back only with the signature that proves it the
        // model's own; without one it is presentation and stays here.
        Content::Reasoning {
            text,
            extra_content,
        } => {
            let kept = extra(extra_content.as_ref(), EXTRA)?;
            if let Some(data) = kept.get("data").and_then(Value::as_str) {
                json!({"type": "redacted_thinking", "data": data})
            } else {
                let signature = kept.get("signature").and_then(Value::as_str)?;
                json!({"type": "thinking", "thinking": text, "signature": signature})
            }
        }
    })
}

/// Whether Anthropic's model `id` refuses a request that forces a tool call
/// (a tool choice of `any`, or of one tool by name): its newest models do,
/// and take `auto` alone.
fn refuses_forced_tools(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    [
        "claude-fable-5-1",
        "claude-mythos-5-1",
        "claude-opus-5-5",
        "claude-sonnet-5-5",
    ]
    .iter()
    .any(|family| id.contains(family))
}

pub(super) fn response(value: &Value) -> Result<ModelResponse, ModelError> {
    let invalid = |why: &str| ModelError::invalid(format!("the provider's answer {why}"));
    if value.get("role").is_some_and(|role| role != "assistant") {
        return Err(invalid("was not the assistant's"));
    }
    let blocks = value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("had no content"))?;
    let mut content = Vec::new();
    for block in blocks {
        let text = |key: &str| block.get(key).and_then(Value::as_str);
        match block.get("type").and_then(Value::as_str) {
            Some("text") => content.push(Content::Text {
                text: text("text")
                    .ok_or_else(|| invalid("had a text block without text"))?
                    .into(),
            }),
            Some("thinking") => content.push(Content::Reasoning {
                text: text("thinking").unwrap_or_default().into(),
                extra_content: text("signature")
                    .filter(|signature| !signature.is_empty())
                    .map(|signature| json!({ EXTRA: {"signature": signature} })),
            }),
            Some("redacted_thinking") => content.push(Content::Reasoning {
                text: String::new(),
                extra_content: text("data").map(|data| json!({ EXTRA: {"data": data} })),
            }),
            Some("tool_use") => {
                let (Some(id), Some(name), Some(input)) =
                    (text("id"), text("name"), block.get("input"))
                else {
                    return Err(invalid("had a tool call without its id, name or input"));
                };
                let call = ToolCall {
                    id: id.into(),
                    name: name.into(),
                    input: input.clone(),
                    extra_content: None,
                };
                call.validate()?;
                content.push(Content::ToolCall(call));
            }
            _ => {}
        }
    }
    let stop_reason = match value.get("stop_reason").and_then(Value::as_str) {
        Some("tool_use") => StopReason::ToolCalls,
        Some("max_tokens" | "model_context_window_exceeded") => StopReason::OutputLimit,
        Some("refusal") => StopReason::Refusal,
        Some("end_turn" | "stop_sequence" | "pause_turn") | None => {
            if content
                .iter()
                .any(|block| matches!(block, Content::ToolCall(_)))
            {
                StopReason::ToolCalls
            } else {
                StopReason::EndTurn
            }
        }
        Some(_) => return Err(invalid("ended for a reason no provider names")),
    };
    let usage = value.get("usage");
    let cached_input_tokens = count(usage, "/cache_read_input_tokens");
    let cache_write_tokens = count(usage, "/cache_creation_input_tokens");
    let input_tokens = count(usage, "/input_tokens")
        .saturating_add(cached_input_tokens)
        .saturating_add(cache_write_tokens);
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
            total_tokens: input_tokens.saturating_add(output_tokens),
            cached_input_tokens,
            cache_write_tokens,
            reasoning_tokens: 0,
        },
    })
}

/// A streamed Messages answer folded into the message it stands for. The
/// stream must open with one `message_start` and close with one
/// `message_stop`; anything else is a cut or malformed answer.
#[derive(Default)]
pub(super) struct Fold {
    message: Option<Value>,
    /// Each block as it is built, with a tool block's arguments as text.
    blocks: Vec<(Value, String)>,
    /// Which tool call each content index is (for its deltas).
    calls: Vec<usize>,
    stopped: bool,
}

impl Fold {
    #[expect(
        clippy::too_many_lines,
        reason = "each event of the Messages stream in turn, read top to bottom"
    )]
    pub(super) fn event(&mut self, event: &Value, out: &mut Vec<Delta>) -> Result<(), String> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if kind == "error" {
            return Err(event.to_string());
        }
        if kind == "ping" {
            return Ok(());
        }
        if self.stopped {
            return Err("the provider wrote after its message ended".into());
        }
        if kind == "message_start" {
            if self.message.is_some() {
                return Err("the provider opened its message twice".into());
            }
            let message = event
                .get("message")
                .filter(|message| message.is_object())
                .ok_or("the provider's message had no envelope")?;
            self.message = Some(message.clone());
            return Ok(());
        }
        let message = self
            .message
            .as_mut()
            .ok_or("the provider wrote before opening its message")?;
        let index = || {
            event
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
        };
        match kind {
            "content_block_start" => {
                let index = index().ok_or("a block without an index")?;
                if index != self.blocks.len() || index >= super::sse::MOST_BLOCKS {
                    return Err("a block out of order".into());
                }
                let block = event
                    .get("content_block")
                    .filter(|block| block.is_object())
                    .ok_or("a block without content")?
                    .clone();
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    let call = self.calls.len();
                    self.calls.push(index);
                    out.push(Delta::CallOpened {
                        index: call,
                        id: block.get("id").and_then(Value::as_str).map(str::to_owned),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                    });
                }
                self.blocks.push((block, String::new()));
            }
            "content_block_delta" => {
                let index = index().ok_or("a delta without an index")?;
                let (block, arguments) = self
                    .blocks
                    .get_mut(index)
                    .ok_or("a delta for a block never opened")?;
                let delta = event.get("delta").ok_or("a delta without content")?;
                let piece = |key: &str| delta.get(key).and_then(Value::as_str).unwrap_or_default();
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        append(block, "text", piece("text"));
                        out.push(Delta::Text(piece("text").into()));
                    }
                    Some("thinking_delta") => {
                        append(block, "thinking", piece("thinking"));
                        out.push(Delta::Reasoning(piece("thinking").into()));
                    }
                    Some("signature_delta") => append(block, "signature", piece("signature")),
                    Some("input_json_delta") => {
                        arguments.push_str(piece("partial_json"));
                        let call = self
                            .calls
                            .iter()
                            .position(|at| *at == index)
                            .ok_or("arguments for a block that is no tool call")?;
                        out.push(Delta::CallArguments {
                            index: call,
                            json: piece("partial_json").into(),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = index().ok_or("a block end without an index")?;
                let (block, _) = self
                    .blocks
                    .get(index)
                    .ok_or("the end of a block never opened")?;
                if let Some(call) = self.calls.iter().position(|at| *at == index) {
                    out.push(Delta::CallClosed {
                        index: call,
                        id: block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                    });
                }
            }
            "message_delta" => {
                let message = message
                    .as_object_mut()
                    .ok_or("the provider's message is no object")?;
                if let Some(delta) = event.get("delta").and_then(Value::as_object) {
                    for (key, value) in delta {
                        message.insert(key.clone(), value.clone());
                    }
                }
                if let Some(usage) = event.get("usage").and_then(Value::as_object) {
                    let kept = message.entry("usage").or_insert_with(|| json!({}));
                    if !kept.is_object() {
                        *kept = json!({});
                    }
                    for (key, value) in usage {
                        put(kept, key, value.clone());
                    }
                }
            }
            "message_stop" => self.stopped = true,
            _ => {}
        }
        Ok(())
    }

    /// Whether the message was opened and closed.
    pub(super) fn finished(&self) -> bool {
        self.stopped
    }

    /// The whole message.
    pub(super) fn answer(self) -> Result<Value, String> {
        let mut message = self.message.ok_or("the provider sent no message")?;
        let mut content = Vec::new();
        for (mut block, arguments) in self.blocks {
            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                let input = if arguments.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&arguments)
                        .map_err(|_| "a tool call's arguments are not JSON")?
                };
                put(&mut block, "input", input);
            }
            content.push(block);
        }
        put(&mut message, "content", Value::Array(content));
        Ok(message)
    }
}

fn append(block: &mut Value, key: &str, piece: &str) {
    let kept = block.get(key).and_then(Value::as_str).unwrap_or_default();
    let joined = format!("{kept}{piece}");
    put(block, key, Value::String(joined));
}

/// Anthropic's limit on `cache_control` breakpoints in one request.
const MAX_CACHE_BREAKPOINTS: usize = 4;

/// Mark an Anthropic request's stable prefixes for prompt caching.
///
/// Anthropic caches only up to an explicit `cache_control` breakpoint (an
/// OpenAI-compatible provider caches a repeated prefix by itself), so without
/// these every agent turn re-prefills the tools, the system prompt and the
/// whole conversation. In order of value, each only while a breakpoint is
/// left and only where none is already set:
///
/// 1. the last `system` block (a string becomes one text block): tools and
///    system render first, so this one entry holds both, across turns and
///    tasks;
/// 2. the last cacheable block of the last `user` message, so the growing
///    conversation is read back incrementally on the next turn — skipped
///    when the caller asked for automatic caching (top-level
///    `cache_control`), which places that breakpoint itself;
/// 3. the first of several `system` blocks: stable instructions kept apart
///    from the system context after them (a planner's apps), which still
///    holds when that context changes;
/// 4. the first block of the first user message with at least two text
///    blocks: the catalogue boundary preserved separately from the task;
/// 5. the last tool, which still holds when the system prompt changes.
///
/// A caller's own breakpoints count against the limit and are never moved or
/// duplicated. A caller that chose a non-default TTL owns the whole layout (a
/// shorter-lived entry may not precede a longer one), so nothing is added. A
/// profile listing `cache_control` in `omit_params` (an Anthropic-compatible
/// endpoint that rejects it) gets no breakpoints and loses a top-level
/// `cache_control`, as every omitted field does.
pub(super) fn cache_breakpoints(profile: &ProviderProfile, object: &mut Map<String, Value>) {
    if profile
        .omit_params
        .iter()
        .any(|param| param == "cache_control")
    {
        object.remove("cache_control");
        return;
    }
    let existing = cache_markers(object);
    if existing.iter().any(|marker| {
        marker
            .get("ttl")
            .is_some_and(|ttl| ttl.as_str() != Some("5m"))
    }) {
        return;
    }
    let mut free = MAX_CACHE_BREAKPOINTS.saturating_sub(existing.len());
    let automatic = object.contains_key("cache_control");
    if free > 0 && object.get_mut("system").is_some_and(mark_last_cacheable) {
        free = free.saturating_sub(1);
    }
    if free > 0 && !automatic && mark_conversation_tail(object) {
        free = free.saturating_sub(1);
    }
    let system = object.get_mut("system").and_then(Value::as_array_mut);
    let several =
        |blocks: &&mut Vec<_>| blocks.iter().filter(|block| cacheable_block(block)).count() > 1;
    if let Some(stable) = system
        .filter(several)
        .and_then(|blocks| blocks.first_mut())
        .filter(|block| free > 0 && cacheable_block(block) && block.get("cache_control").is_none())
    {
        put(stable, "cache_control", json!({ "type": "ephemeral" }));
        free = free.saturating_sub(1);
    }
    if free > 0 && mark_catalogue_boundary(object) {
        free = free.saturating_sub(1);
    }
    if free > 0 {
        mark_last_tool(object);
    }
}

/// Every `cache_control` already in an Anthropic request: top-level
/// (automatic caching), on tools, on system blocks, on message blocks and on
/// the blocks inside a `tool_result`. Structural, so a tool schema or a tool
/// input that merely has a `cache_control` key is not counted.
fn cache_markers(object: &Map<String, Value>) -> Vec<&Value> {
    fn items(value: Option<&Value>) -> impl Iterator<Item = &Value> {
        value.and_then(Value::as_array).into_iter().flatten()
    }
    let mut markers: Vec<&Value> = object.get("cache_control").into_iter().collect();
    for item in items(object.get("tools")).chain(items(object.get("system"))) {
        markers.extend(item.get("cache_control"));
    }
    for message in items(object.get("messages")) {
        for block in items(message.get("content")) {
            markers.extend(block.get("cache_control"));
            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                for inner in items(block.get("content")) {
                    markers.extend(inner.get("cache_control"));
                }
            }
        }
    }
    markers
}

/// Whether a content block may carry `cache_control`. Thinking blocks cannot,
/// and an empty text block is refused with or without a marker.
fn cacheable_block(block: &Value) -> bool {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty()),
        Some("image" | "document" | "search_result" | "tool_use" | "tool_result") => true,
        _ => false,
    }
}

/// Put a breakpoint on the last cacheable block of `content` (a `system`
/// value or a message's `content`), turning a plain string into one text
/// block. False when there is no such block or it is already marked.
fn mark_last_cacheable(content: &mut Value) -> bool {
    match content {
        Value::String(text) if !text.trim().is_empty() => {
            let text = std::mem::take(text);
            *content = json!([{
                "type": "text",
                "text": text,
                "cache_control": { "type": "ephemeral" },
            }]);
            true
        }
        Value::Array(blocks) => {
            let Some(block) = blocks
                .iter_mut()
                .rev()
                .find(|block| cacheable_block(block))
                .and_then(Value::as_object_mut)
            else {
                return false;
            };
            if block.contains_key("cache_control") {
                return false;
            }
            block.insert("cache_control".into(), json!({ "type": "ephemeral" }));
            true
        }
        _ => false,
    }
}

/// Put a breakpoint at the end of the last `user` message.
fn mark_conversation_tail(object: &mut Map<String, Value>) -> bool {
    object
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .and_then(|messages| {
            messages
                .iter_mut()
                .rev()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        })
        .and_then(|message| message.get_mut("content"))
        .is_some_and(mark_last_cacheable)
}

/// Cache the catalogue kept as the first text block of the first user turn,
/// separately from its task. Single-block and non-text prefixes stay untouched.
fn mark_catalogue_boundary(object: &mut Map<String, Value>) -> bool {
    let Some(blocks) = object
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .and_then(|messages| {
            messages
                .iter_mut()
                .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        })
        .and_then(|message| message.get_mut("content"))
        .and_then(Value::as_array_mut)
    else {
        return false;
    };
    if blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .take(2)
        .count()
        < 2
    {
        return false;
    }
    let Some(block) = blocks.first_mut().filter(|block| {
        block.get("type").and_then(Value::as_str) == Some("text")
            && cacheable_block(block)
            && block.get("cache_control").is_none()
    }) else {
        return false;
    };
    put(block, "cache_control", json!({ "type": "ephemeral" }));
    true
}

/// Put a breakpoint on the last tool sent with the request (a deferred tool
/// is not part of the prompt prefix).
fn mark_last_tool(object: &mut Map<String, Value>) {
    let Some(tool) = object
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|tools| {
            tools
                .iter_mut()
                .rev()
                .find(|tool| tool.get("defer_loading").and_then(Value::as_bool) != Some(true))
        })
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    tool.entry("cache_control")
        .or_insert_with(|| json!({ "type": "ephemeral" }));
}
