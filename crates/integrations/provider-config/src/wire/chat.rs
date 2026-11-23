//! `OpenAI` chat completions: the request, a whole answer, and the chunks of
//! a streamed one folded back into a whole answer.

use serde_json::{Map, Value, json};

use super::{Delta, count, extra, put};
use crate::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall,
    ToolChoice, Usage,
};

/// The extension key a reasoning block keeps the field it arrived in under,
/// so it goes back in that field: `DeepSeek` refuses a tool turn whose
/// replayed message lacks its `reasoning_content`.
const FIELD: &str = "chat_field";
/// The fields an assistant message carries shown reasoning in, in the order
/// they are read.
const REASONING_FIELDS: [&str; 3] = ["reasoning_content", "reasoning", "reasoning_text"];

mod thought;

pub(super) fn body(request: &ModelRequest) -> Result<Map<String, Value>, ModelError> {
    let mut messages = Vec::new();
    for message in &request.messages {
        messages_of(message, &mut messages)?;
    }
    let mut object = Map::new();
    object.insert("messages".into(), Value::Array(messages));
    if !request.tools.is_empty() && request.tool_choice != ToolChoice::None {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    }
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

fn messages_of(message: &Message, out: &mut Vec<Value>) -> Result<(), ModelError> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut images = false;
    let mut calls = Vec::new();
    let mut reasoning: Vec<(String, String)> = Vec::new();
    for content in &message.content {
        match content {
            Content::Text { text: piece } => {
                text.push_str(piece);
                parts.push(json!({"type": "text", "text": piece}));
            }
            Content::Image { media_type, base64 } => {
                images = true;
                parts.push(json!({
                    "type": "image_url",
                    "image_url": {"url": format!("data:{media_type};base64,{base64}")},
                }));
            }
            Content::ToolCall(call) => {
                let mut sent = json!({
                    "id": call.id,
                    "type": "function",
                    "function": {
                        "name": call.name,
                        "arguments": serde_json::to_string(&call.input)
                            .map_err(|_| ModelError::invalid("unwritable tool arguments"))?,
                    },
                });
                // Gemini signs each call and refuses a next turn whose
                // replayed call lacks its `thought_signature`.
                if let Some(extra) = call
                    .extra_content
                    .as_ref()
                    .filter(|extra| extra.is_object())
                {
                    put(&mut sent, "extra_content", extra.clone());
                }
                calls.push(sent);
            }
            Content::ToolResult(result) => out.push(json!({
                "role": "tool",
                "tool_call_id": result.call_id,
                "content": result.content,
            })),
            Content::Reasoning {
                text: thought,
                extra_content,
            } => {
                if let Some(field) = extra(extra_content.as_ref(), FIELD)
                    .and_then(Value::as_str)
                    .and_then(|field| REASONING_FIELDS.into_iter().find(|known| *known == field))
                {
                    match reasoning.iter_mut().find(|(name, _)| name == field) {
                        Some((_, kept)) => kept.push_str(thought),
                        None => reasoning.push((field.into(), thought.clone())),
                    }
                }
            }
        }
    }
    let content = if images {
        Value::Array(parts)
    } else if text.is_empty() {
        Value::Null
    } else {
        Value::String(text)
    };
    let role = match message.role {
        Role::System => "system",
        Role::Assistant => "assistant",
        // Results went out as their own `tool` messages above.
        Role::Tool if content.is_null() => return Ok(()),
        Role::User | Role::Tool => "user",
    };
    let mut sent = json!({"role": role, "content": content});
    if role == "assistant" && !calls.is_empty() {
        put(&mut sent, "tool_calls", Value::Array(calls));
    } else if content.is_null() && reasoning.is_empty() {
        return Ok(());
    }
    if role == "assistant" {
        for (field, text) in reasoning {
            put(&mut sent, &field, Value::String(text));
        }
    }
    out.push(sent);
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "one answer read top to bottom: reasoning, words, calls, stop and usage"
)]
/// The first choice of a chat-completions answer. An unknown finish reason,
/// or a tool call without an id, a name or object arguments, is malformed.
pub(super) fn response(value: &Value, google: bool) -> Result<ModelResponse, ModelError> {
    let invalid = |why: &str| ModelError::invalid(format!("the provider's answer {why}"));
    let message = value
        .pointer("/choices/0/message")
        .filter(|message| message.is_object())
        .ok_or_else(|| invalid("had no message"))?;
    if message.get("role").is_some_and(|role| role != "assistant") {
        return Err(invalid("was not the assistant's"));
    }
    let mut content = Vec::new();
    for field in REASONING_FIELDS {
        match message.get(field) {
            None | Some(Value::Null) => {}
            Some(Value::String(text)) if text.is_empty() => {}
            Some(Value::String(text)) => content.push(Content::Reasoning {
                text: text.clone(),
                extra_content: Some(json!({ FIELD: field })),
            }),
            Some(_) => return Err(invalid("had reasoning that was not text")),
        }
    }
    let mut refused = message
        .get("refusal")
        .and_then(Value::as_str)
        .is_some_and(|refusal| !refusal.is_empty());
    // A message its provider marks as a thought is all thought.
    let all_thought = thought::marked(message);
    let said = |text: &str, thinking: bool| {
        if thinking {
            Content::Reasoning {
                text: text.into(),
                extra_content: None,
            }
        } else {
            Content::Text { text: text.into() }
        }
    };
    match message.get("content") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => {
            // At Google's endpoint a thought may stand between tags at the content's head.
            let (thinking, answer) = if google && !all_thought {
                thought::split(text)
            } else {
                (None, text.as_str())
            };
            if let Some(thinking) = thinking.filter(|thinking| !thinking.is_empty()) {
                content.push(said(thinking, true));
            }
            if !answer.is_empty() {
                content.push(said(answer, all_thought));
            }
        }
        Some(Value::Array(parts)) => {
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            content.push(said(text, all_thought || thought::part(part)));
                        }
                    }
                    Some("refusal") => refused = true,
                    _ => {}
                }
            }
        }
        Some(_) => return Err(invalid("had content that was not text")),
    }
    let calls = match message.get("tool_calls") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(calls)) => calls.as_slice(),
        Some(_) => return Err(invalid("had malformed tool calls")),
    };
    for call in calls {
        let field = |pointer: &str| call.pointer(pointer).and_then(Value::as_str);
        let (Some(id), Some(name), Some(arguments)) = (
            field("/id"),
            field("/function/name"),
            field("/function/arguments"),
        ) else {
            return Err(invalid("had a tool call without its id, name or arguments"));
        };
        let input: Value = serde_json::from_str(arguments)
            .map_err(|_| invalid("had tool arguments that are not JSON"))?;
        let call = ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            extra_content: call
                .get("extra_content")
                .filter(|extra| extra.is_object())
                .cloned(),
        };
        call.validate()?;
        content.push(Content::ToolCall(call));
    }
    let stop_reason = match value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
    {
        _ if refused => StopReason::Refusal,
        Some("tool_calls" | "function_call") => StopReason::ToolCalls,
        Some("length") => StopReason::OutputLimit,
        Some("content_filter") => StopReason::Refusal,
        None => return Err(invalid("did not say why it ended")),
        Some("stop") => {
            if calls.is_empty() {
                StopReason::EndTurn
            } else {
                StopReason::ToolCalls
            }
        }
        Some(_) => return Err(invalid("ended for a reason no provider names")),
    };
    let usage = value.get("usage");
    let input_tokens = count(usage, "/prompt_tokens");
    let output_tokens = count(usage, "/completion_tokens");
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
            cached_input_tokens: count(usage, "/prompt_tokens_details/cached_tokens"),
            cache_write_tokens: 0,
            reasoning_tokens: count(usage, "/completion_tokens_details/reasoning_tokens"),
        },
    })
}

/// A streamed chat answer folded into the whole answer it stands for.
#[derive(Default)]
pub(super) struct Fold {
    /// Whether the answer comes from Google's endpoint, whose thought may stand between tags
    /// at the head of its content ([`thought`]).
    google: bool,
    thoughts: thought::Thoughts,
    /// The thought its content carried, apart from the answer.
    thought: String,
    content: String,
    refusal: String,
    reasoning: Vec<(&'static str, String)>,
    calls: Vec<Call>,
    finish: Option<String>,
    usage: Option<Value>,
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
    extra: Option<Value>,
    opened: bool,
}

impl Fold {
    /// A fold of an answer from Google's endpoint when `google`.
    pub(super) fn new(google: bool) -> Self {
        Self {
            google,
            ..Self::default()
        }
    }

    /// Whether the answer comes from Google's endpoint.
    pub(super) const fn google(&self) -> bool {
        self.google
    }

    /// `text` of the content, a thought's when `thinking`.
    fn said(&mut self, text: &str, thinking: bool, out: &mut Vec<Delta>) {
        if text.is_empty() {
            return;
        }
        if thinking {
            self.thought.push_str(text);
            out.push(Delta::Reasoning(text.into()));
        } else {
            self.content.push_str(text);
            out.push(Delta::Text(text.into()));
        }
    }

    /// One chunk. A chunk carrying an `error` is the provider failing.
    pub(super) fn chunk(&mut self, chunk: &Value, out: &mut Vec<Delta>) -> Result<(), String> {
        if chunk.get("error").is_some() {
            return Err(chunk.to_string());
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object()) {
            self.usage = Some(usage.clone());
        }
        // Only the first choice is the turn; a chunk of another is not.
        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| {
                choices
                    .iter()
                    .find(|choice| choice.get("index").is_none_or(|index| index == 0))
            })
        else {
            return Ok(());
        };
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(reason.into());
        }
        let Some(delta) = choice.get("delta") else {
            return Ok(());
        };
        for field in REASONING_FIELDS {
            if let Some(text) = delta.get(field).and_then(Value::as_str)
                && !text.is_empty()
            {
                match self.reasoning.iter_mut().find(|(name, _)| *name == field) {
                    Some((_, kept)) => kept.push_str(text),
                    None => self.reasoning.push((field, text.into())),
                }
                out.push(Delta::Reasoning(text.into()));
                break;
            }
        }
        let all_thought = thought::marked(delta);
        match delta.get("content") {
            Some(Value::String(text)) if all_thought => self.said(text, true, out),
            Some(Value::String(text)) if self.google => {
                for (thinking, piece) in self.thoughts.take(text) {
                    self.said(&piece, thinking, out);
                }
            }
            Some(Value::String(text)) => self.said(text, false, out),
            Some(Value::Array(parts)) => {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        self.said(text, all_thought || thought::part(part), out);
                    }
                }
            }
            _ => {}
        }
        if let Some(text) = delta.get("refusal").and_then(Value::as_str) {
            self.refusal.push_str(text);
        }
        for fragment in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.call(fragment, out)?;
        }
        Ok(())
    }

    /// One fragment of a tool call. Its name may come in pieces (some
    /// providers split it, others restate it whole in every chunk): an
    /// empty piece says nothing, a restatement of the name so far adds
    /// nothing, and any other piece is appended. The name is final only
    /// when the turn finishes.
    fn call(&mut self, fragment: &Value, out: &mut Vec<Delta>) -> Result<(), String> {
        let index = fragment.get("index").and_then(Value::as_u64).map_or_else(
            || self.calls.len().saturating_sub(1),
            |index| usize::try_from(index).unwrap_or(usize::MAX),
        );
        if index > self.calls.len() || index >= super::sse::MOST_CALLS {
            return Err("a tool call out of order".into());
        }
        if index == self.calls.len() {
            self.calls.push(Call::default());
        }
        let call = self
            .calls
            .get_mut(index)
            .ok_or("a tool call out of order")?;
        if let Some(id) = fragment.get("id").and_then(Value::as_str)
            && !id.is_empty()
        {
            id.clone_into(&mut call.id);
        }
        if let Some(name) = fragment.pointer("/function/name").and_then(Value::as_str)
            && !name.is_empty()
            && name != call.name
        {
            call.name.push_str(name);
        }
        if let Some(extra) = fragment
            .get("extra_content")
            .filter(|extra| extra.is_object())
        {
            call.extra = Some(extra.clone());
        }
        if !call.opened {
            call.opened = true;
            out.push(Delta::CallOpened {
                index,
                id: (!call.id.is_empty()).then(|| call.id.clone()),
                name: String::new(),
            });
        }
        if let Some(arguments) = fragment
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            && !arguments.is_empty()
        {
            call.arguments.push_str(arguments);
            out.push(Delta::CallArguments {
                index,
                json: arguments.into(),
            });
        }
        Ok(())
    }

    /// Whether the provider said why the turn ended.
    pub(super) fn finished(&self) -> bool {
        self.finish.is_some()
    }

    /// The whole answer, and each call closed.
    pub(super) fn answer(mut self, out: &mut Vec<Delta>) -> Value {
        if let Some((thinking, held)) = self.thoughts.end() {
            self.said(&held, thinking, out);
        }
        // A thought goes in a part marked as one, which the whole answer reads apart.
        let content = match (self.thought.is_empty(), self.content.is_empty()) {
            (true, true) => Value::Null,
            (true, false) => Value::String(self.content),
            (false, answered) => {
                let mut parts =
                    vec![json!({"type": "text", "text": self.thought, "thought": true})];
                if !answered {
                    parts.push(json!({"type": "text", "text": self.content}));
                }
                Value::Array(parts)
            }
        };
        let mut message = json!({"role": "assistant", "content": content});
        if !self.refusal.is_empty() {
            put(&mut message, "refusal", Value::String(self.refusal));
        }
        for (field, text) in self.reasoning {
            put(&mut message, field, Value::String(text));
        }
        if !self.calls.is_empty() {
            let mut calls = Vec::new();
            for (index, call) in self.calls.into_iter().enumerate() {
                out.push(Delta::CallClosed {
                    index,
                    id: call.id.clone(),
                    name: call.name.clone(),
                });
                let mut sent = json!({
                    "id": call.id,
                    "type": "function",
                    "function": {"name": call.name, "arguments": call.arguments},
                });
                if let Some(extra) = call.extra {
                    put(&mut sent, "extra_content", extra);
                }
                calls.push(sent);
            }
            put(&mut message, "tool_calls", Value::Array(calls));
        }
        let mut answer = json!({
            "object": "chat.completion",
            "choices": [{"index": 0, "message": message, "finish_reason": self.finish}],
        });
        if let Some(usage) = self.usage {
            put(&mut answer, "usage", usage);
        }
        answer
    }
}
