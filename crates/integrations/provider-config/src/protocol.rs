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
        /// Opaque provider state that carries the reasoning into the next
        /// turn, such as `OpenAI` Responses' encrypted reasoning; never shown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra_content: Option<Value>,
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
        let required = |key: &str| {
            schema
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|required| required.iter().any(|name| name == key))
        };
        for (key, value) in object {
            // An optional argument a model writes as `null` is one it left out: the schemas
            // declare no `null` type (every provider's dialect takes them), and a model filling
            // every field it was shown writes the ones it means to omit as `null`.
            if value.is_null() && !required(key) {
                continue;
            }
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
    /// Every input token: uncached, read from a cache and written to one.
    pub input_tokens: u64,
    /// Every output token, reasoning included.
    pub output_tokens: u64,
    pub total_tokens: u64,
    /// The part of `input_tokens` read from the provider's prompt cache.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cached_input_tokens: u64,
    /// The part of `input_tokens` written to the provider's prompt cache.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write_tokens: u64,
    /// The part of `output_tokens` spent reasoning.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reasoning_tokens: u64,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_zero(count: &u64) -> bool {
    *count == 0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    pub message: Message,
    pub stop_reason: StopReason,
    #[serde(default)]
    pub usage: Usage,
    /// What the gateway charged for this answer, in millionths of the
    /// currency, at its price table's list price for the model: the number a
    /// caller spends its own budget against. Zero for a model the table has
    /// no price for (a local model, or an endpoint's own hosting of one).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cost_micros: u64,
}

impl ModelResponse {
    /// Validate the entire turn before exposing any calls, in source order.
    /// A truncated turn is never executable, even if an earlier call parsed.
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
    /// No model provider is set up for this person yet: nothing failed,
    /// there is just nothing to ask.
    NoProvider,
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
    /// This computer can't reach the server that keeps the person's
    /// workspace, where their model is asked: there is no model while it is
    /// offline, and nothing on this computer answers instead.
    Offline,
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
            ErrorKind::Offline => {
                "No model while offline: your model is asked on your workspace's server, which \
                 this computer can't reach right now"
            }
            ErrorKind::NoProvider => "No model provider is set up yet",
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
    /// Set up a model provider (an API key) in Model Providers.
    AddProvider,
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
    /// What may happen next, as an agent's own instruction. It names no
    /// session, setting or vocabulary a person would not know: a person
    /// reads the page's own sentence for the failure, never this.
    pub fn guidance(self) -> &'static str {
        match self {
            Self::AddProvider => "a provider with a model must be set up",
            Self::RefreshSettings => "the provider settings must be read again",
            Self::SignIn => "the provider credential must be checked",
            Self::AddCredits => "the provider needs credit",
            Self::WaitAndRetry => "the provider's rate limit has not reset yet",
            Self::ReduceContext => "the conversation has more context than the model takes",
            Self::RetryInference => "the turn starts again from committed history",
            Self::ContinueText => "the partial answer may be continued as text",
            Self::ChangeRequest => "the request or the model must change",
        }
    }
}

impl ModelError {
    pub fn recovery(&self) -> Recovery {
        match self.kind {
            ErrorKind::Conflict => Recovery::RefreshSettings,
            ErrorKind::NoProvider => Recovery::AddProvider,
            ErrorKind::Authentication => Recovery::SignIn,
            ErrorKind::CreditsExhausted => Recovery::AddCredits,
            ErrorKind::RateLimited => Recovery::WaitAndRetry,
            ErrorKind::ContextLimit => Recovery::ReduceContext,
            ErrorKind::Truncated => Recovery::ContinueText,
            ErrorKind::Unavailable
            | ErrorKind::Offline
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
                        Content::Reasoning { text: part, .. } => reasoning.push_str(part),
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
                    Content::Reasoning { text, .. } => {
                        sink(StreamEvent::ReasoningDelta { text: text.clone() });
                    }
                    _ => {}
                }
            }
            Ok(response)
        })
    }
}
