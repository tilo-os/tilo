//! The scripted model: the task says which tools the turn calls.
//!
//! A task written as a JSON list of calls (`[{"name": "tilo_do", "input":
//! {…}}]`) is called one call a round, and once every call was made the turn
//! answers with what the last one said, so a test reads the tools' own words.
//! A task written any other way has no calls and answers that there is
//! nothing to do. No network, and the same answer to the same task every
//! time: for tests and for a host started with `--agent-model scripted`.

use std::time::Instant;

use tilo_model_gateway::host_backend::protocol::{
    Content, Message, ModelError, ModelRequest, ModelResponse, Role, StopReason, ToolCall, Usage,
};

use super::turns::PlanTurns;

/// A model that does what its request's task writes out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScriptedTurns;

impl PlanTurns for ScriptedTurns {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        _: Instant,
        _: bool,
    ) -> Result<ModelResponse, ModelError> {
        let calls = script(request);
        let made = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, Role::Tool))
            .count();
        let (content, stop_reason) = match calls.get(made) {
            Some(call) => (
                vec![Content::ToolCall(ToolCall {
                    id: format!("call-{made}"),
                    name: call.name.clone(),
                    input: call.input.clone(),
                    extra_content: None,
                })],
                StopReason::ToolCalls,
            ),
            None => (
                vec![Content::Text {
                    text: request
                        .messages
                        .iter()
                        .filter(|message| matches!(message.role, Role::Tool))
                        .next_back()
                        .map_or_else(|| "Nothing to do.".to_owned(), |answered| words(answered)),
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
        "scripted"
    }
}

/// One call the task wrote out.
struct Call {
    name: String,
    input: serde_json::Value,
}

/// The calls the request's task writes out, in order: none when the task is
/// not such a list.
fn script(request: &ModelRequest) -> Vec<Call> {
    let task = request
        .messages
        .iter()
        .find(|message| matches!(message.role, Role::User))
        .map(words)
        .unwrap_or_default();
    let Ok(written) = serde_json::from_str::<Vec<serde_json::Value>>(&task) else {
        return Vec::new();
    };
    written
        .into_iter()
        .filter_map(|call| {
            Some(Call {
                name: call.get("name")?.as_str()?.to_owned(),
                input: call.get("input").cloned().unwrap_or_default(),
            })
        })
        .collect()
}

/// The words and tool results `message` holds.
fn words(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            Content::Text { text } => Some(text.as_str()),
            Content::ToolResult(result) => Some(result.content.as_str()),
            _ => None,
        })
        .collect()
}
