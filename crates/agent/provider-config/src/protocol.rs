//! Serializable model conversation shared by frontends and the model gateway.
//!
//! These are not provider wire envelopes. Credentials, endpoints and HTTP do
//! not belong here. Stream deltas are presentation data; only a validated final
//! response may be handed to an action dispatcher. Retrying inference never
//! authorizes replaying a tool result or an action.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: ToolChoice,
    pub max_output_tokens: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    Auto,
    Required,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![Content::Text { text: text.into() }],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    /// A provider-exposed reasoning summary, not hidden chain-of-thought.
    /// Presentation only: never interpreted as an instruction or tool call.
    Reasoning {
        text: String,
    },
    /// Inline bytes, never a local path or an implicit URL fetch.
    Image {
        media_type: String,
        base64: String,
    },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

/// The same schema is used to advertise a tool and validate its arguments.
/// Business validation and permission checks remain with the tool dispatcher.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl ToolDefinition {
    /// Validate the structural vocabulary used by the advertised tool schemas.
    /// Permission and application-specific checks still belong to dispatch.
    ///
    /// # Errors
    /// Returns an invalid-response error when arguments do not match the schema.
    pub fn validate_input(&self, input: &Value) -> Result<(), ModelError> {
        validate_input(input, &self.input_schema)
            .map_err(|reason| ModelError::invalid(format!("{}: {reason}", self.name)))
    }
}

fn validate_input(input: &Value, schema: &Value) -> Result<(), String> {
    if let Some(variants) = schema.get("anyOf").and_then(Value::as_array)
        && !variants
            .iter()
            .any(|variant| validate_input(input, variant).is_ok())
    {
        return Err("arguments do not match any allowed type".into());
    }
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        let valid = match kind {
            "object" => input.is_object(),
            "array" => input.is_array(),
            "string" => input.is_string(),
            "boolean" => input.is_boolean(),
            "null" => input.is_null(),
            "integer" => input.is_i64() || input.is_u64(),
            "number" => input.is_number(),
            _ => return Err("unsupported schema type".into()),
        };
        if !valid {
            return Err(format!("expected {kind}"));
        }
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.contains(input)
    {
        return Err("value is not one of the advertised choices".into());
    }
    if let Some(object) = input.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(format!("missing {key}"));
                }
            }
        }
        for (key, value) in object {
            if let Some(property) = schema.get("properties").and_then(|p| p.get(key)) {
                validate_input(value, property).map_err(|error| format!("{key}: {error}"))?;
            } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                return Err(format!("unexpected argument {key}"));
            }
        }
    }
    if let Some(items) = input.as_array() {
        if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
            && items.len() as u64 > max
        {
            return Err("too many items".into());
        }
        if let Some(item_schema) = schema.get("items") {
            for item in items {
                validate_input(item, item_schema)?;
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
    /// Opaque provider extensions, including Gemini thought signatures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<Value>,
}

impl ToolCall {
    /// # Errors
    /// Rejects missing identifiers, missing names, and non-object arguments.
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.id.trim().is_empty()
            || self.name.trim().is_empty()
            || !self.input.is_object()
            || self
                .extra_content
                .as_ref()
                .is_some_and(|extra| !extra.is_object())
        {
            return Err(ModelError::invalid(
                "tool calls need an id, a name and an argument object",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolCalls,
    OutputLimit,
    Refusal,
}

/// Normalized token accounting. Missing provider counters remain zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub message: Message,
    pub stop_reason: StopReason,
    #[serde(default)]
    pub usage: Usage,
}

impl ModelResponse {
    /// Validate the entire turn before exposing any calls, in source order.
    /// A truncated turn is never executable, even if an earlier call parsed.
    ///
    /// # Errors
    /// Rejects refused, truncated, malformed, or duplicate tool calls.
    pub fn tool_calls(&self) -> Result<Vec<&ToolCall>, ModelError> {
        if self.stop_reason == StopReason::Refusal {
            return Err(ModelError {
                kind: ErrorKind::Refused,
                message: "the model declined the request".into(),
            });
        }
        if self.stop_reason == StopReason::OutputLimit {
            return Err(ModelError {
                kind: ErrorKind::Truncated,
                message: "the model turn was truncated; no actions from this turn were run".into(),
            });
        }
        if self.message.role != Role::Assistant {
            return Err(ModelError::invalid(
                "model response was not an assistant message",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut calls = Vec::new();
        for content in &self.message.content {
            if let Content::ToolCall(call) = content {
                call.validate()?;
                if !ids.insert(&call.id) {
                    return Err(ModelError::invalid("duplicate tool call id"));
                }
                calls.push(call);
            }
        }
        Ok(calls)
    }
}

/// Portable conversation history. A turn is appended atomically only when
/// every result matches its call, in order. Reusing an executed call id is
/// refused rather than silently replaying an action after an inference retry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    messages: Vec<Message>,
}

impl Conversation {
    pub fn new(system: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            messages: vec![
                Message::text(Role::System, system),
                Message::text(Role::User, user),
            ],
        }
    }

    pub fn request(&self, tools: Vec<ToolDefinition>, max_output_tokens: u32) -> ModelRequest {
        ModelRequest {
            messages: self.messages.clone(),
            tool_choice: if tools.is_empty() {
                ToolChoice::None
            } else {
                ToolChoice::Required
            },
            tools,
            max_output_tokens,
        }
    }

    /// Commit a validated turn and its matching tool results.
    ///
    /// # Errors
    /// Rejects invalid turns, replayed calls, or missing and mismatched results.
    pub fn push(
        &mut self,
        response: ModelResponse,
        results: Vec<ToolResult>,
    ) -> Result<(), ModelError> {
        let calls = self.validate_response(&response)?;
        if calls.len() != results.len()
            || calls
                .iter()
                .zip(&results)
                .any(|(call, result)| call.id != result.call_id)
        {
            return Err(ModelError::invalid(
                "tool results must match every call in order",
            ));
        }
        self.messages.push(response.message);
        if !results.is_empty() {
            self.messages.push(Message {
                role: Role::Tool,
                content: results.into_iter().map(Content::ToolResult).collect(),
            });
        }
        Ok(())
    }

    /// Call before dispatch as well as before committing the results.
    ///
    /// # Errors
    /// Rejects invalid turns and call identifiers already present in history.
    pub fn validate_response<'a>(
        &self,
        response: &'a ModelResponse,
    ) -> Result<Vec<&'a ToolCall>, ModelError> {
        let calls = response.tool_calls()?;
        let previous: BTreeSet<&str> = self
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| {
                if let Content::ToolCall(call) = content {
                    Some(call.id.as_str())
                } else {
                    None
                }
            })
            .collect();
        if calls.iter().any(|call| previous.contains(call.id.as_str())) {
            return Err(ModelError::invalid("tool call id was already consumed"));
        }
        Ok(calls)
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
}

/// Presentation-only updates. A call delta, including a complete-looking JSON
/// argument string, must never trigger dispatch. `Finished` is authoritative.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    TextDelta {
        text: String,
    },
    /// Only summaries explicitly exposed by the provider. Never synthesized.
    ReasoningDelta {
        text: String,
    },
    ToolCallDelta {
        index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Finished {
        response: ModelResponse,
    },
    Failed {
        error: ModelError,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Unavailable,
    Authentication,
    PermissionDenied,
    InvalidRequest,
    /// Provider descriptions changed since the administration snapshot.
    Conflict,
    InvalidResponse,
    Unsupported,
    RateLimited,
    /// Billing or quota exhaustion; retrying without funding cannot help.
    CreditsExhausted,
    /// The conversation exceeds the selected model's context window.
    ContextLimit,
    /// A provider failure not classified more specifically.
    Provider,
    TimedOut,
    Cancelled,
    Truncated,
    Refused,
    Transport,
}

/// Safe frontend diagnostic. Backends must not place credentials or raw
/// provider bodies in `message`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelError {
    pub kind: ErrorKind,
    pub message: String,
}

impl ModelError {
    /// Stable diagnostic for failures whose source may contain credentials or
    /// provider-controlled text. Never echo that source into a frontend error.
    pub fn new(kind: ErrorKind) -> Self {
        let message = match kind {
            ErrorKind::Unavailable => "Model service unavailable",
            ErrorKind::Authentication => {
                "Provider authentication failed; check the credential in Settings"
            }
            ErrorKind::PermissionDenied => "Provider permission denied",
            ErrorKind::InvalidRequest => "Invalid model request",
            ErrorKind::Conflict => "Provider settings changed; refresh before saving",
            ErrorKind::InvalidResponse => "Invalid model response",
            ErrorKind::Unsupported => "Model operation unsupported; update the gateway",
            ErrorKind::RateLimited => "Provider rate limit reached",
            ErrorKind::CreditsExhausted => "Provider credits exhausted",
            ErrorKind::ContextLimit => "Model context limit exceeded",
            ErrorKind::Provider => "Provider request failed",
            ErrorKind::TimedOut => "Model request timed out",
            ErrorKind::Cancelled => "Model request cancelled",
            ErrorKind::Truncated => {
                "Model stream ended without a final response; partial output retained"
            }
            ErrorKind::Refused => "Model declined the request",
            ErrorKind::Transport => "Model network transport failed",
        };
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::InvalidResponse,
            message: message.into(),
        }
    }
}

/// What a person can do next. None of these authorize replaying actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    /// Reload the authoritative snapshot; do not overwrite concurrent edits.
    RefreshSettings,
    SignIn,
    AddCredits,
    WaitAndRetry,
    ReduceContext,
    /// Start fresh inference from committed history, never from a partial call.
    RetryInference,
    /// Explicitly request continuation of prose; partial tool calls are discarded.
    ContinueText,
    ChangeRequest,
}

impl Recovery {
    pub fn guidance(self) -> &'static str {
        match self {
            Self::RefreshSettings => {
                "Refresh provider settings and review the current values before saving again."
            }
            Self::SignIn => "Check the provider credential in Settings, then try again.",
            Self::AddCredits => "Add provider credits or select another funded provider.",
            Self::WaitAndRetry => {
                "Wait for the provider rate limit to reset, then retry inference."
            }
            Self::ReduceContext => {
                "Shorten the conversation or select a model with a larger context window."
            }
            Self::RetryInference => {
                "Retry inference from saved history; do not replay completed actions."
            }
            Self::ContinueText => {
                "Continue the partial answer as text, or retry inference; no partial actions were run."
            }
            Self::ChangeRequest => {
                "Review the request or select another model before trying again."
            }
        }
    }
}

impl ModelError {
    pub fn recovery(&self) -> Recovery {
        match self.kind {
            ErrorKind::Conflict => Recovery::RefreshSettings,
            ErrorKind::Authentication => Recovery::SignIn,
            ErrorKind::CreditsExhausted => Recovery::AddCredits,
            ErrorKind::RateLimited => Recovery::WaitAndRetry,
            ErrorKind::ContextLimit => Recovery::ReduceContext,
            ErrorKind::Truncated => Recovery::ContinueText,
            ErrorKind::Unavailable
            | ErrorKind::TimedOut
            | ErrorKind::Transport
            | ErrorKind::Provider => Recovery::RetryInference,
            _ => Recovery::ChangeRequest,
        }
    }
}

/// Explicit terminal status: transport EOF is never completion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamStatus {
    #[default]
    Streaming,
    Completed,
    Truncated,
    Refused,
    Cancelled,
    Failed,
}

/// Retained presentation state for one inference attempt. Deltas are inert;
/// only `response()` exposes a validated, completed turn for dispatch. There is
/// deliberately no automatic retry: callers keep committed conversation/results
/// and start new inference rather than re-executing a previous action batch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamState {
    pub status: StreamStatus,
    pub text: String,
    pub reasoning: String,
    pub error: Option<ModelError>,
    final_response: Option<ModelResponse>,
}

impl StreamState {
    /// Terminal states are sticky, including cancellation racing a late finish.
    pub fn apply(&mut self, event: &StreamEvent) {
        if self.status != StreamStatus::Streaming {
            return;
        }
        match event {
            StreamEvent::TextDelta { text } => self.text.push_str(text),
            StreamEvent::ReasoningDelta { text } => self.reasoning.push_str(text),
            StreamEvent::ToolCallDelta { .. } => {}
            StreamEvent::Failed { error } => {
                self.status = match error.kind {
                    ErrorKind::Cancelled => StreamStatus::Cancelled,
                    ErrorKind::Truncated => StreamStatus::Truncated,
                    ErrorKind::Refused => StreamStatus::Refused,
                    _ => StreamStatus::Failed,
                };
                self.error = Some(error.clone());
            }
            StreamEvent::Finished { response } => {
                let validation = response.tool_calls();
                if let Err(error) = &validation
                    && !matches!(error.kind, ErrorKind::Truncated | ErrorKind::Refused)
                {
                    self.fail(error.clone());
                    return;
                }
                // Finals are authoritative where present; absent fields must not
                // erase partial output on a refusal or truncated response.
                let mut text = String::new();
                let mut reasoning = String::new();
                for block in &response.message.content {
                    match block {
                        Content::Text { text: part } => text.push_str(part),
                        Content::Reasoning { text: part } => reasoning.push_str(part),
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    self.text = text;
                }
                if !reasoning.is_empty() {
                    self.reasoning = reasoning;
                }
                match validation {
                    Ok(_) => {
                        self.status = StreamStatus::Completed;
                        self.final_response = Some(response.clone());
                    }
                    Err(error) => self.apply(&StreamEvent::Failed { error }),
                }
            }
        }
    }

    /// Called on EOF, missing final frame or watchdog expiration. Partial
    /// prose and reasoning remain available, but no calls become executable.
    pub fn fail(&mut self, error: ModelError) {
        self.apply(&StreamEvent::Failed { error });
    }

    pub fn cancel(&mut self) {
        self.fail(ModelError {
            kind: ErrorKind::Cancelled,
            message: "Generation cancelled; partial output retained".into(),
        });
    }

    pub fn response(&self) -> Option<&ModelResponse> {
        self.final_response.as_ref()
    }

    /// Explicit prose-only continuation. Uses committed history plus retained
    /// answer text, never partial tool arguments or reasoning. Tools are disabled
    /// so continuation cannot replay an action. The original history is unchanged.
    ///
    /// # Errors
    /// Rejects states without a truncated, nonempty prose answer.
    pub fn continue_text(
        &self,
        history: &Conversation,
        max_output_tokens: u32,
    ) -> Result<ModelRequest, ModelError> {
        if self.status != StreamStatus::Truncated || self.text.is_empty() {
            return Err(ModelError::invalid(
                "only truncated answer text can be continued",
            ));
        }
        let mut request = history.request(Vec::new(), max_output_tokens);
        request
            .messages
            .push(Message::text(Role::Assistant, &self.text));
        request.messages.push(Message::text(
            Role::User,
            "Continue the interrupted answer from where it stopped. Do not repeat or perform actions.",
        ));
        Ok(request)
    }
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ModelError {}

/// No sockets, threads or `Send` requirement: a browser frontend can implement
/// this using a lightweight backend, while native callers use the gateway.
pub type CompletionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelResponse, ModelError>> + 'a>>;

/// Local presentation sink. Tool deltas are never authorization to execute.
pub type StreamSink<'a> = Box<dyn FnMut(StreamEvent) + 'a>;

pub trait ModelBackend {
    fn complete<'a>(&'a self, request: &'a ModelRequest) -> CompletionFuture<'a>;

    /// Backends may override for live deltas. Dropping the future cancels the
    /// local subscription. The returned response, not EOF, is authoritative.
    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        mut sink: StreamSink<'a>,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            let response = self.complete(request).await?;
            for content in &response.message.content {
                match content {
                    Content::Text { text } => sink(StreamEvent::TextDelta { text: text.clone() }),
                    Content::Reasoning { text } => {
                        sink(StreamEvent::ReasoningDelta { text: text.clone() });
                    }
                    _ => {}
                }
            }
            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(ids: &[&str]) -> ModelResponse {
        ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: ids
                    .iter()
                    .map(|id| {
                        Content::ToolCall(ToolCall {
                            id: (*id).into(),
                            name: "invoke".into(),
                            input: json!({"enabled": false, "count": 2}),
                            extra_content: None,
                        })
                    })
                    .collect(),
            },
            stop_reason: StopReason::ToolCalls,
            usage: Usage::default(),
        }
    }

    #[test]
    fn partial_output_survives_every_non_success_terminal_state() {
        for (kind, status) in [
            (ErrorKind::Truncated, StreamStatus::Truncated),
            (ErrorKind::Refused, StreamStatus::Refused),
            (ErrorKind::Cancelled, StreamStatus::Cancelled),
            (ErrorKind::Transport, StreamStatus::Failed),
            (ErrorKind::TimedOut, StreamStatus::Failed),
            (ErrorKind::InvalidResponse, StreamStatus::Failed),
        ] {
            let mut state = StreamState::default();
            state.apply(&StreamEvent::TextDelta {
                text: "Partial answer".into(),
            });
            state.apply(&StreamEvent::ReasoningDelta {
                text: "Exposed summary".into(),
            });
            state.apply(&StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("a".into()),
                name: Some("invoke".into()),
                arguments: "{}".into(),
            });
            assert!(state.response().is_none());
            state.fail(ModelError {
                kind,
                message: "interrupted".into(),
            });
            // Late success and late deltas cannot resurrect an interrupted turn.
            state.apply(&StreamEvent::Finished {
                response: response(&["a"]),
            });
            state.apply(&StreamEvent::TextDelta {
                text: "late".into(),
            });
            assert_eq!(state.status, status);
            assert_eq!(state.text, "Partial answer");
            assert_eq!(state.reasoning, "Exposed summary");
            assert!(state.response().is_none());
        }
    }

    #[test]
    fn authoritative_stop_reason_controls_dispatch_and_continuation() {
        let history = Conversation::new("system", "user");
        for (reason, status) in [
            (StopReason::ToolCalls, StreamStatus::Completed),
            (StopReason::OutputLimit, StreamStatus::Truncated),
            (StopReason::Refusal, StreamStatus::Refused),
        ] {
            let mut state = StreamState::default();
            let mut final_response = response(&["a"]);
            final_response.stop_reason = reason;
            final_response.message.content.push(Content::Text {
                text: "Answer".into(),
            });
            final_response.message.content.push(Content::Reasoning {
                text: "Summary".into(),
            });
            state.apply(&StreamEvent::Finished {
                response: final_response,
            });
            assert_eq!(state.status, status);
            assert_eq!(
                state.response().is_some(),
                status == StreamStatus::Completed
            );
            if status == StreamStatus::Truncated {
                let request = state.continue_text(&history, 100).unwrap();
                assert_eq!(request.tool_choice, ToolChoice::None);
                assert!(request.tools.is_empty());
                assert_eq!(
                    request.messages[2],
                    Message::text(Role::Assistant, "Answer")
                );
                assert_eq!(request.messages.len(), 4);
                assert_eq!(history.messages().len(), 2);
                assert!(!serde_json::to_string(&request).unwrap().contains("Summary"));
            } else {
                assert!(state.continue_text(&history, 100).is_err());
            }
            let decoded: StreamState =
                serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
            assert_eq!(decoded, state);
        }
    }

    #[test]
    fn malformed_final_keeps_partial_output_and_is_not_dispatchable() {
        let mut state = StreamState::default();
        state.apply(&StreamEvent::TextDelta {
            text: "partial".into(),
        });
        let mut malformed = response(&["a", "a"]);
        malformed.message.content.push(Content::Text {
            text: "invalid replacement".into(),
        });
        state.apply(&StreamEvent::Finished {
            response: malformed,
        });
        assert_eq!(state.status, StreamStatus::Failed);
        assert_eq!(state.error.unwrap().kind, ErrorKind::InvalidResponse);
        assert_eq!(state.text, "partial");
        assert!(state.final_response.is_none());
    }

    #[test]
    fn recovery_is_actionable_without_automatic_action_replay() {
        for (kind, recovery) in [
            (ErrorKind::Conflict, Recovery::RefreshSettings),
            (ErrorKind::Authentication, Recovery::SignIn),
            (ErrorKind::CreditsExhausted, Recovery::AddCredits),
            (ErrorKind::RateLimited, Recovery::WaitAndRetry),
            (ErrorKind::ContextLimit, Recovery::ReduceContext),
            (ErrorKind::TimedOut, Recovery::RetryInference),
            (ErrorKind::Transport, Recovery::RetryInference),
            (ErrorKind::Provider, Recovery::RetryInference),
            (ErrorKind::Truncated, Recovery::ContinueText),
            (ErrorKind::InvalidResponse, Recovery::ChangeRequest),
        ] {
            assert_eq!(
                ModelError {
                    kind,
                    message: String::new()
                }
                .recovery(),
                recovery
            );
        }
    }

    #[test]
    fn usage_round_trips_and_older_responses_default_to_zero() {
        let mut answer = response(&["a"]);
        answer.usage = Usage {
            input_tokens: 12,
            output_tokens: 3,
            total_tokens: 15,
        };
        let encoded = serde_json::to_value(&answer).unwrap();
        assert_eq!(
            serde_json::from_value::<ModelResponse>(encoded.clone()).unwrap(),
            answer
        );
        let mut older = encoded;
        older.as_object_mut().unwrap().remove("usage");
        assert_eq!(
            serde_json::from_value::<ModelResponse>(older)
                .unwrap()
                .usage,
            Usage::default()
        );
    }

    #[test]
    fn tool_schema_checks_nested_types_and_unknown_arguments() {
        let tool = ToolDefinition {
            name: "test".into(),
            description: String::new(),
            input_schema: json!({
                "type": "object", "required": ["steps"], "additionalProperties": false,
                "properties": {"steps": {"type": "array", "items": {
                    "type": "object", "required": ["enabled"],
                    "properties": {"enabled": {"type": "boolean"}}
                }}}
            }),
        };
        assert!(
            tool.validate_input(&json!({"steps": [{"enabled": false}]}))
                .is_ok()
        );
        for input in [
            json!({}),
            json!({"steps": [{"enabled": "false"}]}),
            json!({"steps": [], "extra": true}),
        ] {
            assert!(tool.validate_input(&input).is_err());
        }
    }

    #[test]
    fn complete_calls_keep_ids_order_and_argument_types() {
        let original = response(&["second", "first"]);
        let decoded: ModelResponse =
            serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(decoded, original);
        let calls = decoded.tool_calls().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|call| call.id.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );
        assert_eq!(calls[0].input["enabled"], false);
        assert_eq!(calls[0].input["count"], 2);
    }

    #[test]
    fn history_is_atomic_and_retries_cannot_replay_consumed_calls() {
        let mut history = Conversation::new("system", "user");
        let answer = response(&["a", "b"]);
        let result = |id: &str| ToolResult {
            call_id: id.into(),
            content: "ok".into(),
            is_error: false,
        };
        assert!(
            history
                .push(answer.clone(), vec![result("b"), result("a")])
                .is_err()
        );
        assert_eq!(history.messages().len(), 2);
        history
            .push(answer.clone(), vec![result("a"), result("b")])
            .unwrap();
        assert_eq!(history.messages().len(), 4);
        assert!(
            history
                .push(answer, vec![result("a"), result("b")])
                .is_err()
        );
        assert_eq!(history.messages().len(), 4);
    }

    #[test]
    fn malformed_duplicate_and_truncated_calls_are_not_dispatchable() {
        assert!(response(&["same", "same"]).tool_calls().is_err());
        assert!(response(&[""]).tool_calls().is_err());
        let mut partial = response(&["valid"]);
        partial.stop_reason = StopReason::OutputLimit;
        assert!(partial.tool_calls().is_err());
        let Content::ToolCall(call) = &mut partial.message.content[0] else {
            panic!()
        };
        call.input = json!("not an object");
        partial.stop_reason = StopReason::ToolCalls;
        assert!(partial.tool_calls().is_err());
    }
}
