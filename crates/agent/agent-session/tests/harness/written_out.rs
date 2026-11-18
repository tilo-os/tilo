//! A model a test writes out: the task's own text is the list of tool calls
//! its turn makes.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]
#![allow(
    clippy::needless_pass_by_value,
    reason = "a call is written as the model reads it, its arguments whole"
)]

use std::time::Instant;

use tilo_agent_session::model::PlanTurns;
use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage,
};

/// A model that does what its request writes out: a request that is a JSON
/// list of tool calls (`[{"name": "tilo_do", "input": {…}}]`) is called one
/// call a round, and once every call was made it answers with what the last
/// one said, so the task's answer is the tools' own.
pub struct WrittenOut;

impl PlanTurns for WrittenOut {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        let words = |message: &Message| {
            message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::Text { text } => Some(text.clone()),
                    Content::ToolResult(result) => Some(result.content.clone()),
                    _ => None,
                })
                .collect::<String>()
        };
        let asked = request
            .messages
            .iter()
            .find(|message| matches!(message.role, Role::User))
            .map(words)
            .unwrap_or_default();
        let calls: Vec<serde_json::Value> = serde_json::from_str(&asked).unwrap_or_default();
        let answered: Vec<&Message> = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, Role::Tool))
            .collect();
        let (content, stop_reason) = match calls.get(answered.len()) {
            Some(call) => (
                vec![Content::ToolCall(ToolCall {
                    id: format!("call-{}", answered.len()),
                    name: call["name"].as_str().unwrap_or_default().to_owned(),
                    input: call["input"].clone(),
                    extra_content: None,
                })],
                StopReason::ToolCalls,
            ),
            None => (
                vec![Content::Text {
                    text: answered
                        .last()
                        .map_or_else(|| "Nothing to do.".to_owned(), |last| words(last)),
                }],
                StopReason::EndTurn,
            ),
        };
        Ok(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content,
            },
            stop_reason,
            usage: Usage::default(),
            cost_micros: 0,
        })
    }

    fn model(&self) -> &'static str {
        "written-out"
    }
}

/// One tool call, as [`WrittenOut`] makes it.
pub fn call(name: &str, input: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"name": name, "input": input})
}

/// A `tilo_do` call: do the action `path` names with `args`.
pub fn doing(path: &str, args: serde_json::Value) -> serde_json::Value {
    call("tilo_do", serde_json::json!({"path": path, "args": args}))
}

/// [`doing`] with the arguments an action's own names made.
pub fn doing_args(
    path: &str,
    args: serde_json::Map<String, serde_json::Value>,
) -> serde_json::Value {
    doing(path, serde_json::Value::Object(args))
}

/// The task asking for `calls`, in order.
pub fn task(calls: &[serde_json::Value]) -> String {
    serde_json::Value::Array(calls.to_vec()).to_string()
}

/// The model a server's agents work with: the task one is given is the list
/// of tool calls its turn makes ([`WrittenOut`]).
#[must_use]
pub fn model() -> tilo_agent_session::MakeTurns {
    std::sync::Arc::new(|| Some(Box::new(WrittenOut) as Box<dyn PlanTurns>))
}
