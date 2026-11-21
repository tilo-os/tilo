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
                        .rfind(|message| matches!(message.role, Role::Tool))
                        .map_or_else(|| "Nothing to do.".to_owned(), words),
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

#[cfg(test)]
mod tests {

    use super::*;
    use tilo_model_gateway::host_backend::protocol::ToolChoice;

    fn asking(task: &str) -> ModelRequest {
        ModelRequest {
            messages: vec![
                Message::text(Role::System, "you are an agent"),
                Message::text(Role::User, task),
            ],
            tools: Vec::new(),
            tool_choice: ToolChoice::default(),
            max_output_tokens: 1_024,
        }
    }

    /// The message a tool's answer comes in.
    fn answered(call_id: &str, content: &str) -> Message {
        Message {
            role: Role::Tool,
            content: vec![
                serde_json::from_value(serde_json::json!({
                    "type": "tool_result",
                    "call_id": call_id,
                    "content": content,
                    "is_error": false,
                }))
                .unwrap(),
            ],
        }
    }

    #[test]
    fn a_task_written_out_as_calls_is_called_one_a_round_then_answered_with_the_tools_words() {
        let mut model = ScriptedTurns;
        let mut request = asking(
            r#"[{"name": "tilo_do", "input": {"path": "notes#new-note", "args": {"title": "a"}}}]"#,
        );
        let first = model
            .turn(&request, "key:0", Instant::now(), false)
            .unwrap();
        assert_eq!(first.stop_reason, StopReason::ToolCalls);
        assert_eq!(
            first.message.content,
            vec![Content::ToolCall(ToolCall {
                id: "call-0".into(),
                name: "tilo_do".into(),
                input: serde_json::json!({"path": "notes#new-note", "args": {"title": "a"}}),
                extra_content: None,
            })]
        );

        request.messages.push(first.message);
        request.messages.push(answered("call-0", "made the note"));
        let answered = model
            .turn(&request, "key:1", Instant::now(), false)
            .unwrap();
        assert_eq!(answered.stop_reason, StopReason::EndTurn);
        assert_eq!(words(&answered.message), "made the note");
    }

    #[test]
    fn a_task_that_writes_out_no_calls_answers_that_there_is_nothing_to_do() {
        let mut model = ScriptedTurns;
        let answered = model
            .turn(&asking("say hello"), "key:0", Instant::now(), false)
            .unwrap();
        assert_eq!(answered.stop_reason, StopReason::EndTurn);
        assert_eq!(words(&answered.message), "Nothing to do.");
    }
}
