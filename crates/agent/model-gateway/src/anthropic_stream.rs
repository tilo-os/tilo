//! Anthropic Messages streaming.
//!
//! A streamed Messages answer is a sequence of server-sent events —
//! `message_start`, then for each content block `content_block_start`,
//! `content_block_delta`s and `content_block_stop`, then `message_delta`
//! (stop reason and final usage) and `message_stop`, with `ping`s anywhere.
//! [`MessageAssembly`] folds them back into the body a non-streamed request
//! returns, and keeps each `tool_use` block's input text as a
//! [`StreamCall`], complete at its own `content_block_stop`, so the gateway
//! reports a call while the model is still writing the next one.
//!
//! An `error` event is not folded here: the stream reader refuses any event
//! carrying an `error` object, for both dialects.

use serde_json::{Map, Value};

use crate::{ProviderFailure, StreamCall, stream_failure};

/// What one event changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Folded {
    /// It carried model output: text, a tool call starting, or tool input.
    pub(crate) output: bool,
    /// It was `message_stop`: the stream is over.
    pub(crate) stop: bool,
}

/// One content block as it streams.
struct Block {
    /// The block as `content_block_start` gave it, with deltas applied.
    value: Map<String, Value>,
    /// `input_json_delta` text of a block that is not a tool call (a server
    /// tool's input). A tool call's text is its [`StreamCall::arguments`].
    json: String,
    /// The block's position among the message's tool calls, if it is one.
    call: Option<usize>,
    closed: bool,
}

/// Rebuilds an Anthropic Messages body from its stream events.
#[derive(Default)]
pub(crate) struct MessageAssembly {
    /// `message_start`'s message: id, model, role, opening usage, ….
    message: Option<Map<String, Value>>,
    blocks: Vec<Block>,
    /// The message's `tool_use` blocks, in order.
    pub(crate) calls: Vec<StreamCall>,
    /// Tool calls below this index are complete.
    complete: usize,
    /// `message_delta` fields: `stop_reason`, `stop_sequence`.
    delta: Map<String, Value>,
    /// `message_delta` usage, which supersedes the opening counts.
    usage: Map<String, Value>,
    stopped: bool,
    pub(crate) reasoning_chunks: u64,
    pub(crate) reasoning_bytes: u64,
}

fn unreadable() -> ProviderFailure {
    stream_failure("provider stream was unreadable")
}

/// A `content_block_*` event's block index.
fn block_index(event: &Value) -> Result<usize, ProviderFailure> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .and_then(|index| usize::try_from(index).ok())
        .ok_or_else(unreadable)
}

/// Append `text` to the string field `key` of `block`.
fn append(block: &mut Map<String, Value>, key: &str, text: &str) {
    match block.get_mut(key) {
        Some(Value::String(existing)) => existing.push_str(text),
        _ => {
            block.insert(key.into(), Value::String(text.into()));
        }
    }
}

impl MessageAssembly {
    /// Fold one event in.
    pub(crate) fn fold(&mut self, event: &Value) -> Result<Folded, ProviderFailure> {
        let mut folded = Folded::default();
        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let message = event
                    .get("message")
                    .and_then(Value::as_object)
                    .filter(|_| self.message.is_none())
                    .ok_or_else(unreadable)?;
                self.message = Some(message.clone());
            }
            Some("content_block_start") => folded.output = self.start_block(event)?,
            Some("content_block_delta") => folded.output = self.block_delta(event)?,
            Some("content_block_stop") => self.stop_block(event)?,
            Some("message_delta") => {
                for (field, into) in [("delta", &mut self.delta), ("usage", &mut self.usage)] {
                    if let Some(values) = event.get(field).and_then(Value::as_object) {
                        into.extend(
                            values
                                .iter()
                                .map(|(key, value)| (key.clone(), value.clone())),
                        );
                    }
                }
            }
            Some("message_stop") => {
                self.stopped = true;
                folded.stop = true;
            }
            // `ping`, and event kinds added after this was written.
            _ => {}
        }
        Ok(folded)
    }

    fn start_block(&mut self, event: &Value) -> Result<bool, ProviderFailure> {
        let index = block_index(event)?;
        let value = event
            .get("content_block")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(unreadable)?;
        // Blocks arrive one at a time, in index order, each after the last
        // one closed; anything else cannot be assembled faithfully.
        if self.message.is_none()
            || index != self.blocks.len()
            || self.blocks.last().is_some_and(|block| !block.closed)
        {
            return Err(unreadable());
        }
        let call = if value.get("type").and_then(Value::as_str) == Some("tool_use") {
            let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
            let (Some(id), Some(name)) = (text("id"), text("name")) else {
                return Err(unreadable());
            };
            self.calls.push(StreamCall {
                id,
                name,
                arguments: String::new(),
            });
            Some(self.calls.len().saturating_sub(1))
        } else {
            None
        };
        self.blocks.push(Block {
            value,
            json: String::new(),
            call,
            closed: false,
        });
        Ok(call.is_some())
    }

    fn block_delta(&mut self, event: &Value) -> Result<bool, ProviderFailure> {
        let index = block_index(event)?;
        let Some(block) = self.blocks.get_mut(index).filter(|block| !block.closed) else {
            return Err(unreadable());
        };
        let Some(delta) = event.get("delta") else {
            return Err(unreadable());
        };
        let text = |key: &str| delta.get(key).and_then(Value::as_str);
        let output = match delta.get("type").and_then(Value::as_str) {
            Some("text_delta") => {
                let text = text("text").ok_or_else(unreadable)?;
                append(&mut block.value, "text", text);
                !text.is_empty()
            }
            Some("input_json_delta") => {
                let partial = text("partial_json").ok_or_else(unreadable)?;
                match block.call.and_then(|call| self.calls.get_mut(call)) {
                    Some(call) => call.arguments.push_str(partial),
                    None => block.json.push_str(partial),
                }
                !partial.is_empty()
            }
            Some("thinking_delta") => {
                let thinking = text("thinking").ok_or_else(unreadable)?;
                append(&mut block.value, "thinking", thinking);
                if !thinking.is_empty() {
                    self.reasoning_chunks = self.reasoning_chunks.saturating_add(1);
                    self.reasoning_bytes =
                        self.reasoning_bytes.saturating_add(thinking.len() as u64);
                }
                false
            }
            Some("signature_delta") => {
                append(
                    &mut block.value,
                    "signature",
                    text("signature").ok_or_else(unreadable)?,
                );
                false
            }
            Some("citations_delta") => {
                let citation = delta.get("citation").cloned().ok_or_else(unreadable)?;
                match block.value.get_mut("citations") {
                    Some(Value::Array(citations)) => citations.push(citation),
                    _ => {
                        block
                            .value
                            .insert("citations".into(), Value::Array(vec![citation]));
                    }
                }
                false
            }
            // A delta kind added after this was written changes nothing here.
            _ => false,
        };
        Ok(output)
    }

    /// Close a block. A tool call's input is decoded now, and the call is
    /// complete: it can run before the model writes its next block.
    fn stop_block(&mut self, event: &Value) -> Result<(), ProviderFailure> {
        let index = block_index(event)?;
        let Some(block) = self.blocks.get_mut(index).filter(|block| !block.closed) else {
            return Err(unreadable());
        };
        block.closed = true;
        let call = block.call.and_then(|call| self.calls.get_mut(call));
        let text = match &call {
            Some(call) => call.arguments.as_str(),
            None => block.json.as_str(),
        };
        if !text.is_empty() {
            let input = serde_json::from_str::<Value>(text)
                .map_err(|_| stream_failure("provider tool input was not JSON"))?;
            block.value.insert("input".into(), input);
        }
        if let Some(call) = call {
            if call.arguments.is_empty() {
                // No input was streamed: the call takes the block's own
                // (normally empty) input, which its frame then carries.
                call.arguments = block
                    .value
                    .get("input")
                    .map_or_else(|| "{}".into(), Value::to_string);
            }
            self.complete = block
                .call
                .map_or(self.complete, |call| call.saturating_add(1));
        }
        Ok(())
    }

    /// How many leading tool calls are complete.
    pub(crate) fn complete(&self) -> usize {
        self.complete.min(self.calls.len())
    }

    /// Whether `message_stop` arrived.
    pub(crate) const fn stopped(&self) -> bool {
        self.stopped
    }

    /// The message's usage: the opening counts, superseded by the final ones.
    pub(crate) fn usage(&self) -> Option<Value> {
        let mut usage = self
            .message
            .as_ref()
            .and_then(|message| message.get("usage"))
            .and_then(Value::as_object)
            .cloned();
        if !self.usage.is_empty() {
            usage.get_or_insert_default().extend(
                self.usage
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        usage.map(Value::Object)
    }

    /// The Messages body a non-streamed request would have returned.
    pub(crate) fn into_message(self) -> Result<Value, ProviderFailure> {
        let usage = self.usage();
        let Some(mut message) = self.message else {
            return Err(stream_failure("provider stream ended early"));
        };
        if self.blocks.iter().any(|block| !block.closed) {
            return Err(stream_failure("provider stream ended early"));
        }
        message.insert(
            "content".into(),
            Value::Array(
                self.blocks
                    .into_iter()
                    .map(|block| Value::Object(block.value))
                    .collect(),
            ),
        );
        message.extend(self.delta);
        if let Some(usage) = usage {
            message.insert("usage".into(), usage);
        }
        Ok(Value::Object(message))
    }
}
