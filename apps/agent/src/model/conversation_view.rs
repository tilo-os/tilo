//! What a long run's conversation sends: results put aside and turns
//! folded, read off the history and applied when the request is built
//! ([`Conversation::body`]). The run's compaction decides what to put
//! aside; the history itself keeps everything.

use std::collections::HashMap;

use serde_json::Value;

use super::Conversation;
#[cfg(test)]
use super::LegacyConversation;

/// One tool call of the conversation and the result it was answered with.
#[derive(Clone, Debug)]
pub struct Answered {
    pub call_id: String,
    pub tool: String,
    pub input: Value,
    pub result: String,
    /// The index of the history message holding the result.
    pub at: usize,
}

impl Conversation {
    /// Every tool call so far with its result, oldest first: the result as
    /// it was, not as a compaction sends it.
    pub fn answered(&self) -> Vec<Answered> {
        use agent_provider_config::protocol::Content;
        #[cfg(test)]
        if let Some(legacy) = &self.legacy {
            return legacy.answered();
        }
        let mut calls: HashMap<&str, (&str, &Value)> = HashMap::new();
        let mut answered = Vec::new();
        for (at, content) in self
            .history
            .messages()
            .iter()
            .enumerate()
            .flat_map(|(at, message)| message.content.iter().map(move |content| (at, content)))
        {
            match content {
                Content::ToolCall(call) => {
                    calls.insert(&call.id, (&call.name, &call.input));
                }
                Content::ToolResult(result) => {
                    let (tool, input) = calls
                        .get(result.call_id.as_str())
                        .copied()
                        .unwrap_or(("", &Value::Null));
                    answered.push(Answered {
                        call_id: result.call_id.clone(),
                        tool: tool.to_owned(),
                        input: input.clone(),
                        result: result.content.clone(),
                        at,
                    });
                }
                _ => {}
            }
        }
        answered
    }

    /// Send `call_id`'s result as `aside` from now on.
    pub fn put_aside(&mut self, call_id: &str, aside: String) {
        self.compacted.insert(call_id.to_owned(), aside);
    }

    /// Whether `call_id`'s result is put aside.
    pub fn is_aside(&self, call_id: &str) -> bool {
        self.compacted.contains_key(call_id)
    }

    /// Replace the pictures shown before the last direction that showed
    /// any with `note`: a picture is looked at when it is shown, and a long
    /// run need not send it again on every turn. Returns how many were
    /// put aside.
    pub fn put_pictures_aside(&mut self, note: &str) -> usize {
        use agent_provider_config::protocol::Content;
        let showing = |message: &agent_provider_config::protocol::Message| {
            message
                .content
                .iter()
                .any(|content| matches!(content, Content::Image { .. }))
        };
        let Some(last) = self
            .directions
            .iter()
            .rposition(|(_, message)| showing(message))
        else {
            return 0;
        };
        let mut aside = 0;
        for (_, message) in self.directions.iter_mut().take(last) {
            for content in &mut message.content {
                if matches!(content, Content::Image { .. }) {
                    *content = Content::Text { text: note.into() };
                    aside += 1;
                }
            }
        }
        aside
    }
}

#[cfg(test)]
impl LegacyConversation {
    /// Every tool call with its result, oldest first.
    pub(super) fn answered(&self) -> Vec<Answered> {
        let mut calls: HashMap<String, (String, Value)> = HashMap::new();
        let mut answered = Vec::new();
        for (at, message) in self.messages.iter().enumerate() {
            for call in message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
                let function = call.get("function").cloned().unwrap_or_default();
                let name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let input = function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .and_then(|text| serde_json::from_str(text).ok())
                    .unwrap_or_default();
                calls.insert(id.to_owned(), (name.to_owned(), input));
            }
            let mut blocks: Vec<&Value> = match message.get("content") {
                Some(Value::Array(blocks)) => blocks.iter().collect(),
                _ => Vec::new(),
            };
            blocks.push(message);
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    let id = block.get("id").and_then(Value::as_str).unwrap_or_default();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let input = block.get("input").cloned().unwrap_or_default();
                    calls.insert(id.to_owned(), (name.to_owned(), input));
                }
                let id = block
                    .get("tool_call_id")
                    .or_else(|| block.get("tool_use_id"))
                    .and_then(Value::as_str);
                if let Some(id) = id {
                    let (tool, input) = calls.get(id).cloned().unwrap_or_default();
                    answered.push(Answered {
                        call_id: id.to_owned(),
                        tool,
                        input,
                        at,
                        result: block
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    });
                }
            }
        }
        answered
    }
}
