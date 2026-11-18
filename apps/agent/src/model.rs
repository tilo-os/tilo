//! Talking to the model: which published operations answer a request.
//!
//! The agent never composes a document mutation of its own. It is shown the
//! live, permission-filtered catalogue of operations the workspace's applications
//! publish — the same catalogue a person reaches from the palette — and it
//! discovers, invokes and observes through a small set of generic tools
//! ([`crate::tools`]). Every operation id it names is checked against what a
//! tool actually showed it and against the catalogue again at dispatch; the
//! broker is the only authority.
//!
//! One shape of conversation lives here: [`TurnRequest`] / [`TurnEvent`],
//! one streamed model turn of the discover → invoke → observe loop that
//! model tasks run (`session::agent_loop`). The turn runs on its own
//! thread; previews remain inert until the complete response and tool
//! batch validate.
//!
//! Every call is bounded by a timeout; a slow, failed or malformed answer is
//! reported as such rather than guessed at.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use ui::time::Duration;

use serde_json::Value;

use crate::providers::ProviderSelection;
use crate::surface::Catalogue;
use crate::tools::{self, Revealed};

pub use agent_provider_config::protocol::ToolCall;

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
enum ActionTurn {
    Tools {
        /// The assistant turn to replay, in the provider's own dialect.
        assistant: Value,
        calls: Vec<ToolCall>,
    },
    Plan {
        /// The assistant turn and the call's id, to answer a refused plan.
        assistant: Value,
        id: String,
        input: Value,
    },
}

/// The two wire dialects of the action-discovery tool loop. Both run the
/// same conversation with the same bounds: read-only discovery tools, then
/// exactly one `plan` call, alone in its turn, as the final answer.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    /// Anthropic Messages: `system` key, `input_schema` tools, `tool_use`
    /// blocks, `tool_result` blocks in one user turn.
    Anthropic,
    /// `OpenAI` Chat Completions: a system message, `function` tools,
    /// `message.tool_calls` with JSON-string arguments, `role: "tool"` replies.
    OpenAi,
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
impl Dialect {
    fn of(protocol: agent_provider_config::ProviderProtocol) -> Self {
        match protocol {
            agent_provider_config::ProviderProtocol::Anthropic => Self::Anthropic,
            agent_provider_config::ProviderProtocol::OpenAiCompatible => Self::OpenAi,
        }
    }

    fn first_messages(self, first: &FirstTurn) -> Vec<Value> {
        match self {
            Self::Anthropic => vec![first.message(self)],
            Self::OpenAi => vec![
                serde_json::json!({ "role": "system", "content": ACTION_SYSTEM }),
                first.message(self),
            ],
        }
    }

    fn body(self, selection: &ProviderSelection, messages: &[Value], most_steps: usize) -> Value {
        match self {
            Self::Anthropic => action_request_body_for(selection, messages, most_steps),
            Self::OpenAi => openai_action_request_body(selection, messages, most_steps),
        }
    }

    fn parse(self, body: &str) -> Result<ActionTurn, String> {
        match self {
            Self::Anthropic => parse_action_turn(body),
            Self::OpenAi => parse_openai_action_turn(body),
        }
    }

    fn tool_result(self, id: &str, answer: &str) -> Value {
        match self {
            Self::Anthropic => {
                serde_json::json!({ "type": "tool_result", "tool_use_id": id, "content": answer })
            }
            Self::OpenAi => {
                serde_json::json!({ "role": "tool", "tool_call_id": id, "content": answer })
            }
        }
    }
}

/// The system prompt of the action-discovery conversation.
/// Constant, like the tools, so the provider's prompt cache keeps it.
#[cfg(test)]
const ACTION_SYSTEM: &str = "Drive Tilo exclusively through the resource/action tools. The request lists the operations published right now with their parameters: when one of them does what is asked, call `plan` with it straight away. Use the listing tools only for what that list leaves out, and `describe_operation` only when an operation's arguments are unclear from its signature. Preserve every argument's declared JSON type. Never invent an operation id or parameter.";

/// Which path reached the decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// The keyword vocabulary chose, for the stated reason.
    Keywords(Fallback),
}

/// Why the keyword vocabulary had to decide instead of a model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// No key in the environment or the operator's env file.
    NoKey,
    /// Switched off for this run (`--no-model`).
    Disabled,
    /// Transport, HTTP status, a refusal, or an unreadable body.
    Failed(String),
}

/// Selects the gateway-held profile, then answers instructions.
pub struct Chooser {
    /// A Settings-selected provider. Its credential remains in the gateway.
    provider: Option<ProviderConnection>,
    /// Whether its loop speaks the gateway's provider-neutral protocol.
    /// Production transports always do; only a unit-test legacy fixture
    /// keeps provider envelopes.
    neutral: bool,
    timeout: Duration,
    enabled: bool,
    /// The content-free correlation id of the latest action decision, for
    /// the gateway's timing log (0 before the first one).
    trace: AtomicU64,
    /// Where streamed loop turns go: the gateway (set with a provider), or a
    /// scripted transport in tests.
    turns: Option<Arc<dyn TurnTransport>>,
    /// Timing marks wait here while a streamed turn owns the gateway channel.
    marks: Arc<MarkQueue>,
}

struct ProviderConnection {
    selection: ProviderSelection,
}

impl Chooser {
    /// The provider this chooser asks, when it has one.
    pub fn selection(&self) -> Option<&ProviderSelection> {
        self.provider.as_ref().map(|provider| &provider.selection)
    }

    /// The provider and model that plan with this chooser, as a person reads
    /// them, when [`Self::loop_model`] would reach one: the note a run reports
    /// names the same plan the loop takes.
    pub fn planned_by(&self) -> Option<(&str, &str)> {
        self.loop_model().ok()?;
        let selection = self.selection()?;
        Some((
            selection.profile_label.as_str(),
            selection.model.label.as_str(),
        ))
    }

    /// Why a run cannot carry out a task with this chooser's model, naming
    /// the model: it declares no tool calling (`ModelDescriptor::tool_calling`),
    /// so it could never invoke an operation. `None` for a model that calls
    /// tools, and for no model at all, which the loop reports on its own.
    pub fn refuses_tasks(&self) -> Option<String> {
        let selection = self.selection()?;
        (!selection.model.tool_calling).then(|| {
            format!(
                "{} from {} cannot call tools, so it cannot carry out tasks. Choose a model \
                 with tool calling for this agent or this task.",
                selection.model.display_label(),
                selection.profile_label
            )
        })
    }

    /// How many tokens the model this chooser asks reads at once, when its
    /// descriptor knows: the deployment's runtime context first, else the
    /// model's own.
    pub fn context_tokens(&self) -> Option<u32> {
        let model = &self.selection()?.model;
        model
            .request_limits
            .as_ref()
            .and_then(|limits| limits.runtime_context_tokens)
            .or(model.context_window)
    }

    /// The most tokens one answer of the model this chooser asks may take: its
    /// declared output limit, else the default.
    pub fn output_tokens(&self) -> Option<u32> {
        self.selection()
            .map(|selection| selection.model.output_tokens())
    }

    /// The most tokens one answer of the model this chooser asks may take,
    /// only when its descriptor declares it: what compaction leaves room for,
    /// which an undeclared limit must not size by the default request ceiling.
    pub fn declared_output_tokens(&self) -> Option<u32> {
        self.selection()
            .and_then(|selection| selection.model.max_output_tokens)
    }

    /// Whether the model this chooser asks sees pictures.
    pub fn sees(&self) -> bool {
        self.selection()
            .is_some_and(|selection| selection.model.vision)
    }

    /// `enabled` false forces the keyword vocabulary — for a run that must be
    /// reproducible, and for proving the fallback works.
    pub fn new(enabled: bool, timeout: Duration) -> Self {
        Self {
            provider: None,
            neutral: false,
            timeout,
            enabled,
            trace: AtomicU64::new(0),
            turns: None,
            marks: Arc::default(),
        }
    }

    /// Construct a chooser over a profile selected in Settings. The profile
    /// is non-secret metadata; tilo-model-gateway owns credential resolution.
    pub fn with_provider(enabled: bool, timeout: Duration, selection: ProviderSelection) -> Self {
        let marks = Arc::<MarkQueue>::default();
        let turns: Arc<dyn TurnTransport> = Arc::new(GatewayTurns {
            profile_id: selection.profile_id.clone(),
            model_id: selection.model.id.clone(),
            marks: Arc::clone(&marks),
        });
        Self {
            provider: Some(ProviderConnection { selection }),
            neutral: true,
            timeout,
            enabled,
            trace: AtomicU64::new(0),
            turns: Some(turns),
            marks,
        }
    }

    /// A chooser whose loop turns are answered by `turns` instead of the
    /// gateway (tests and offline proofs), selecting `protocol`'s provider.
    pub fn scripted(
        timeout: Duration,
        protocol: agent_provider_config::ProviderProtocol,
        turns: Arc<dyn TurnTransport>,
    ) -> Self {
        let mut profile = agent_provider_config::ProviderProfile::hosted(protocol);
        let model = agent_provider_config::ModelDescriptor::new("scripted");
        profile.models.push(model.clone());
        let selection = ProviderSelection::from_profile(&profile, model);
        Self {
            provider: Some(ProviderConnection { selection }),
            neutral: true,
            timeout,
            enabled: true,
            trace: AtomicU64::new(0),
            turns: Some(turns),
            marks: Arc::default(),
        }
    }

    /// A neutral scripted chooser over `model`, whose descriptor says what
    /// the model declares (its context, tool calling).
    #[cfg(test)]
    pub(crate) fn scripted_model(
        timeout: Duration,
        model: agent_provider_config::ModelDescriptor,
        turns: Arc<dyn TurnTransport>,
    ) -> Self {
        let mut profile = agent_provider_config::ProviderProfile::hosted(
            agent_provider_config::ProviderProtocol::OpenAiCompatible,
        );
        profile.label = "Scripted".into();
        profile.models.push(model.clone());
        let mut chooser = Self::scripted_neutral(timeout, turns);
        chooser.provider = Some(ProviderConnection {
            selection: ProviderSelection::from_profile(&profile, model),
        });
        chooser
    }

    /// A scripted chooser speaking the provider's own dialect `protocol`
    /// names, for a fixture that scripts that dialect's bodies: the legacy
    /// codec a test can still ask for.
    #[cfg(test)]
    pub(crate) fn scripted_dialect(
        timeout: Duration,
        protocol: agent_provider_config::ProviderProtocol,
        turns: Arc<dyn TurnTransport>,
    ) -> Self {
        let mut chooser = Self::scripted(timeout, protocol, turns);
        chooser.neutral = false;
        chooser
    }

    #[cfg(test)]
    pub(crate) fn scripted_neutral(timeout: Duration, turns: Arc<dyn TurnTransport>) -> Self {
        let mut chooser = Self::scripted(
            timeout,
            agent_provider_config::ProviderProtocol::OpenAiCompatible,
            turns,
        );
        chooser.neutral = true;
        chooser
    }

    /// Production transports always use the neutral gateway protocol.
    /// Only unit-test legacy fixtures retain provider envelopes.
    pub fn uses_neutral_protocol(&self) -> bool {
        self.neutral
    }

    /// The per-turn model timeout this chooser was configured with.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Why the discover → invoke → observe loop cannot talk to a model, or
    /// the protocol and model it speaks when it can.
    ///
    /// # Errors
    ///
    /// The fallback route: switched off, or no provider/gateway.
    pub fn loop_model(&self) -> Result<(agent_provider_config::ProviderProtocol, String), Route> {
        if !self.enabled {
            return Err(Route::Keywords(Fallback::Disabled));
        }
        let (Some(provider), Some(_)) = (&self.provider, &self.turns) else {
            return Err(Route::Keywords(Fallback::NoKey));
        };
        Ok((
            provider.selection.protocol,
            provider.selection.model.id.clone(),
        ))
    }

    /// Start a fresh correlation id for one loop (one task).
    pub fn begin_trace(&self) -> u64 {
        let id = uuid::Uuid::new_v4().as_u64_pair().0.max(1);
        self.trace.store(id, Ordering::Relaxed);
        id
    }

    /// Start one streamed turn. Its events arrive on the returned channel,
    /// ending with exactly one [`TurnEvent::Finished`]; dropping the
    /// returned [`TurnStop`] stops the turn.
    pub fn start_turn(
        &self,
        body: Value,
        timeout: Duration,
        turn: usize,
    ) -> (std::sync::mpsc::Receiver<TurnEvent>, TurnStop) {
        let trace = tilo_model_client::PlanTrace {
            id: self.trace.load(Ordering::Relaxed),
            turn: u32::try_from(turn).unwrap_or(u32::MAX),
        };
        let stop = TurnStop::default();
        let rx = if let Some(turns) = &self.turns {
            turns.start(TurnRequest {
                body,
                timeout,
                trace: Some(trace),
                stop: Arc::clone(&stop.0),
            })
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = tx.send(TurnEvent::Finished(Err("no model transport".into())));
            rx
        };
        (rx, stop)
    }

    /// Whether a live call is possible at all. Cheap to ask, and worth
    /// logging at startup so an operator knows before the beat which path
    /// this run will take.
    pub fn is_live(&self) -> bool {
        self.enabled && self.provider.is_some() && tilo_model_client::available()
    }

    /// Record a content-free timing mark for the latest action decision in
    /// the gateway's log (a managed agent's own stderr is discarded). Best
    /// effort and cheap: one local frame; an older gateway's refusal is
    /// ignored.
    pub fn mark(&self, event: tilo_model_client::TraceEvent, step: usize) {
        let id = self.trace.load(Ordering::Relaxed);
        tracing::info!(
            trace = format!("{id:016x}"),
            ?event,
            step,
            "model timing mark"
        );
        if id == 0 || self.provider.is_none() || !tilo_model_client::available() {
            return;
        }
        let trace = tilo_model_client::PlanTrace {
            id,
            turn: u32::try_from(step).unwrap_or(u32::MAX),
        };
        self.marks.mark(trace, event);
    }
}

/// A conversation's first user turn, in two parts: the published operations,
/// the same bytes for the same catalogue whatever the task, then everything
/// that varies per task (the earlier conversation, the document, the
/// request).
///
/// The parts stay separate content blocks: a provider's prompt cache can
/// then read the operations back for the next task over the same catalogue
/// instead of prefilling them again, and caching of that boundary belongs
/// to the gateway.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FirstTurn {
    /// The published operations: stable across tasks.
    pub operations: String,
    /// Everything after them.
    pub task: String,
}

impl FirstTurn {
    pub fn text(&self) -> String {
        format!("{}{}", self.operations, self.task)
    }

    /// Keep the stable catalogue and volatile task in separate content blocks;
    /// provider-specific caching of that boundary belongs to the gateway.
    fn apply_to(&self, request: &mut agent_provider_config::protocol::ModelRequest) {
        use agent_provider_config::protocol::Content;
        let Some(message) = request.messages.get_mut(1) else {
            return;
        };
        message.content = if self.operations.is_empty() {
            vec![Content::Text {
                text: self.task.clone(),
            }]
        } else {
            vec![
                Content::Text {
                    text: self.operations.clone(),
                },
                Content::Text {
                    text: self.task.clone(),
                },
            ]
        };
    }

    #[cfg(test)]
    fn message(&self, dialect: Dialect) -> Value {
        let content = match dialect {
            Dialect::Anthropic if !self.operations.trim().is_empty() => {
                let mut blocks = vec![serde_json::json!({
                    "type": "text",
                    "text": self.operations,
                    "cache_control": { "type": "ephemeral" },
                })];
                if !self.task.trim().is_empty() {
                    blocks.push(serde_json::json!({ "type": "text", "text": self.task }));
                }
                Value::Array(blocks)
            }
            Dialect::Anthropic | Dialect::OpenAi => Value::String(self.text()),
        };
        serde_json::json!({ "role": "user", "content": content })
    }
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
fn fold_anthropic_results(messages: &mut Vec<Value>) {
    let start = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .map_or(0, |index| index + 1);
    let results = messages.split_off(start);
    messages.push(serde_json::json!({ "role": "user", "content": results }));
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
fn action_request_body_for(
    selection: &ProviderSelection,
    messages: &[Value],
    most_steps: usize,
) -> Value {
    serde_json::json!({
        "model": selection.model.id,
        "max_tokens": selection.model.output_tokens(),
        "system": ACTION_SYSTEM,
        "tools": tools::definitions(most_steps),
        "messages": messages,
    })
}

/// The `OpenAI` Chat Completions request for one discovery turn. The tool
/// definitions are the same ones the Anthropic dialect sends, re-enveloped;
/// `tool_choice: "required"` keeps the model inside the tool conversation
/// (an answer that neither discovers nor plans is an error in both dialects).
/// No `reasoning_effort` is chosen here: the gateway applies the profile's.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
fn openai_action_request_body(
    selection: &ProviderSelection,
    messages: &[Value],
    most_steps: usize,
) -> Value {
    serde_json::json!({
        "model": selection.model.id,
        "max_completion_tokens": selection.model.output_tokens(),
        "messages": messages,
        "tools": openai_tools(&tools::definitions(most_steps)),
        "tool_choice": "required",
    })
}

/// `{name, description, input_schema}` → `{type: "function", function:
/// {name, description, parameters}}`.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
fn openai_tools(definitions: &Value) -> Value {
    definitions
        .as_array()
        .into_iter()
        .flatten()
        .map(|definition| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": definition.get("name").cloned().unwrap_or(Value::Null),
                    "description": definition.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": definition
                        .get("input_schema")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({ "type": "object" })),
                }
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
fn parse_action_turn(body: &str) -> Result<ActionTurn, String> {
    let (assistant, calls) = anthropic_calls(body)?;
    settle_turn(assistant, calls)
}

/// The assistant turn (to replay) and its tool calls, in call order.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
fn anthropic_calls(body: &str) -> Result<(Value, Vec<ToolCall>), String> {
    let root: Value = serde_json::from_str(body)
        .map_err(|error| format!("unreadable action response: {error}"))?;
    if root.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
        return Err("the model turn was truncated".into());
    }
    if root.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        return Err("the model declined the action request".into());
    }
    let content = root
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "action response had no content blocks".to_string())?;
    let mut calls = Vec::new();
    for block in &content {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let id = block
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "tool call had no id".to_string())?;
        let name = block
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "tool call had no name".to_string())?;
        let input = block
            .get("input")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        calls.push(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            extra_content: None,
        });
    }
    Ok((
        serde_json::json!({ "role": "assistant", "content": content }),
        calls,
    ))
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
fn parse_openai_action_turn(body: &str) -> Result<ActionTurn, String> {
    let (assistant, calls) = openai_calls(body)?;
    settle_turn(assistant, calls)
}

/// The assistant turn (to replay) and its tool calls, in call order.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
fn openai_calls(body: &str) -> Result<(Value, Vec<ToolCall>), String> {
    let root: Value = serde_json::from_str(body)
        .map_err(|error| format!("unreadable action response: {error}"))?;
    let choice = root
        .pointer("/choices/0")
        .ok_or_else(|| "action response had no choices".to_string())?;
    if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
        return Err("the model turn was truncated".into());
    }
    let message = choice
        .get("message")
        .ok_or_else(|| "action response had no message".to_string())?;
    if choice.get("finish_reason").and_then(Value::as_str) == Some("content_filter")
        || message
            .get("refusal")
            .and_then(Value::as_str)
            .is_some_and(|refusal| !refusal.is_empty())
    {
        return Err("the model declined the action request".into());
    }
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut calls = Vec::new();
    for call in &tool_calls {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "tool call had no id".to_string())?;
        let function = call
            .get("function")
            .ok_or_else(|| "tool call had no function".to_string())?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| "tool call had no name".to_string())?;
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("");
        let input = if arguments.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(arguments)
                .map_err(|_| "tool call arguments were not JSON".to_string())?
        };
        calls.push(ToolCall {
            id: id.into(),
            name: name.into(),
            input,
            extra_content: call
                .get("extra_content")
                .filter(|extra| extra.is_object())
                .cloned(),
        });
    }
    let mut assistant = serde_json::json!({
        "role": "assistant",
        "content": message.get("content").cloned().unwrap_or(Value::Null),
        "tool_calls": tool_calls,
    });
    if let Some(reasoning) = tilo_model_client::validation::reasoning_content(message)?
        && let Some(object) = assistant.as_object_mut()
    {
        object.insert(
            "reasoning_content".into(),
            Value::String(reasoning.to_owned()),
        );
    }
    Ok((assistant, calls))
}

/// The rule shared by both dialects: `plan` is the final answer and must be
/// the only call of its turn; otherwise the turn must call discovery tools.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
fn settle_turn(assistant: Value, calls: Vec<ToolCall>) -> Result<ActionTurn, String> {
    tools::validate_plan_calls(&calls)?;
    if calls.iter().any(|call| call.name == tools::PLAN) {
        let mut calls = calls.into_iter();
        let (Some(call), None) = (calls.next(), calls.next()) else {
            return Err("plan must be the only tool call in its turn".into());
        };
        return Ok(ActionTurn::Plan {
            assistant,
            id: call.id,
            input: call.input,
        });
    }
    if calls.is_empty() {
        return Err("the model neither discovered actions nor submitted a plan".into());
    }
    Ok(ActionTurn::Tools { assistant, calls })
}

/// The first user turn of an action decision: the operations published
/// right now ([`tools::catalogue_summary`], capped as the task loop caps it,
/// each id shown revealed so `plan` may name it at once), then the
/// instruction. Stable first, volatile last: the summary changes only when
/// the applications republish, so it is the [`FirstTurn`]'s cached part and
/// the instruction is its task.
pub fn action_user_turn(rest: &str, catalogue: &Catalogue, revealed: &mut Revealed) -> FirstTurn {
    let summary = tools::catalogue_summary(catalogue, revealed, tools::MOST_SUMMARISED);
    FirstTurn {
        operations: format!("Operations published right now:\n\n{summary}\n"),
        task: format!("The instruction asks for:\n\n{}\n", rest.trim()),
    }
}

/// [`action_user_turn`] for a planner whose resources keep publishing
/// resources as a task runs (clips, sheets, messages): at most `most`
/// operations, with look-alike resources in one line and every resource
/// `rest` names listed whatever the cap
/// ([`tools::catalogue_summary_with`]). `rest` is the request with what the
/// task's steps answered, so an id an earlier step returned may be planned;
/// an offer `described` says the planner was described in full is named
/// without its parameters again.
pub fn action_user_turn_listing(
    rest: &str,
    catalogue: &Catalogue,
    revealed: &mut Revealed,
    most: usize,
    described: &dyn Fn(&crate::surface::Offer) -> bool,
) -> FirstTurn {
    let named = tools::Naming::in_text(rest);
    let summary = tools::catalogue_summary_with(catalogue, revealed, most, Some(&named), described);
    FirstTurn {
        operations: format!("Operations published right now:\n\n{summary}\n"),
        task: format!("The instruction asks for:\n\n{}\n", rest.trim()),
    }
}

// ---------------------------------------------------------------------------
// Streamed loop turns
// ---------------------------------------------------------------------------

/// One event of a streamed loop turn, in arrival order.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnEvent {
    Neutral(agent_provider_config::protocol::StreamEvent),
    /// An intermediate gateway frame: `event` is
    /// [`tilo_model_client::EVENT_TOOL_CALL_DELTA`] or
    /// [`tilo_model_client::EVENT_TOOL_CALL`], `output` its JSON.
    Frame {
        event: String,
        output: String,
    },
    /// The turn is over: the assembled provider body, or why it failed.
    Finished(Result<String, String>),
}

/// One turn to stream.
#[derive(Clone, Debug)]
pub struct TurnRequest {
    pub body: Value,
    pub timeout: Duration,
    pub trace: Option<tilo_model_client::PlanTrace>,
    /// Set once the loop has let go of the turn: the transport stops it.
    pub stop: Arc<AtomicBool>,
}

/// Held by whoever wants a turn's answer: dropping it (a stopped task, a
/// turn the loop lets go of) stops the turn, so a provider call nobody
/// will read is not paid for to its end.
#[derive(Debug, Default)]
pub struct TurnStop(Arc<AtomicBool>);

impl Drop for TurnStop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Runs a turn somewhere else — normally on its own thread over the model
/// gateway — so the loop can execute completed tool calls, publish live
/// activity and read the collaboration link while the model still writes.
pub trait TurnTransport: Send + Sync {
    /// Start `request`; every event goes to the returned receiver, ending
    /// with exactly one [`TurnEvent::Finished`].
    fn start(&self, request: TurnRequest) -> std::sync::mpsc::Receiver<TurnEvent>;
}

/// Timing marks share the gateway's one service channel with plan turns, so
/// a mark sent while a streamed turn owns the channel would interleave with
/// its frames. Marks wait here until the turn is done.
#[derive(Debug, Default)]
pub struct MarkQueue {
    state: Mutex<MarkState>,
}

#[derive(Debug, Default)]
struct MarkState {
    busy: bool,
    /// The turn that owns (or last owned) the channel.
    turn: u64,
    /// That turn's Plan is over: nothing more of it is on the channel.
    over: bool,
    /// Held marks with the Unix ms at which each happened.
    pending: Vec<(
        tilo_model_client::PlanTrace,
        tilo_model_client::TraceEvent,
        u64,
    )>,
}

impl MarkState {
    /// Send the held marks, each with the time it happened, in order.
    fn flush(&mut self) {
        for (trace, event, at) in std::mem::take(&mut self.pending) {
            let _ = tilo_model_client::trace_at(trace, event, Some(at));
        }
    }
}

impl MarkQueue {
    fn mark(&self, trace: tilo_model_client::PlanTrace, event: tilo_model_client::TraceEvent) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.busy {
            if state.pending.len() < 256 {
                let at = ui::time::unix_now_ms();
                state.pending.push((trace, event, at));
            }
            return;
        }
        // Held across the call so a turn cannot start on the channel mid-mark.
        let _ = tilo_model_client::trace(trace, event);
    }

    /// A turn takes the channel; its ticket for [`Self::release`]. Marks the
    /// previous turn held go out first when its Plan is over, even if its
    /// thread has not released yet: the channel is free until this turn's
    /// Plan starts.
    fn acquire(&self) -> u64 {
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        if state.over {
            state.flush();
        }
        state.turn = state.turn.wrapping_add(1);
        state.busy = true;
        state.over = false;
        state.turn
    }

    /// Turn `ticket`'s Plan is over; its `Finished` may go out before the
    /// marks it held.
    fn plan_over(&self, ticket: u64) {
        if let Ok(mut state) = self.state.lock()
            && state.turn == ticket
        {
            state.over = true;
        }
    }

    /// Turn `ticket` is done with the channel: send what it held and free
    /// the channel, unless a later turn already took it.
    fn release(&self, ticket: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.turn != ticket {
            return;
        }
        state.flush();
        state.busy = false;
    }
}

/// Turns over the launcher-issued model gateway.
struct GatewayTurns {
    profile_id: String,
    model_id: String,
    marks: Arc<MarkQueue>,
}

impl TurnTransport for GatewayTurns {
    fn start(&self, request: TurnRequest) -> std::sync::mpsc::Receiver<TurnEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        let profile_id = self.profile_id.clone();
        let model_id = self.model_id.clone();
        let marks = Arc::clone(&self.marks);
        let ticket = marks.acquire();
        let spawned = std::thread::Builder::new()
            .name("agent-model-turn".into())
            .spawn({
                let tx = tx.clone();
                let marks = Arc::clone(&marks);
                move || {
                    let result = if let Ok(neutral) = serde_json::from_value::<
                        agent_provider_config::protocol::ModelRequest,
                    >(request.body.clone())
                    {
                        let mut previews = NeutralPreviews::default();
                        let stop = &request.stop;
                        tilo_model_client::complete_streaming_until(
                            &profile_id,
                            &model_id,
                            neutral,
                            request.timeout,
                            &mut |event| {
                                let _ = tx.send(TurnEvent::Neutral(event.clone()));
                                if let Some(output) = previews.frame(event) {
                                    let _ = tx.send(TurnEvent::Frame {
                                        event: tilo_model_client::EVENT_TOOL_CALL_DELTA.into(),
                                        output,
                                    });
                                }
                            },
                            &|| stop.load(Ordering::Relaxed),
                        )
                        .map(|response| serde_json::json!(response).to_string())
                        .inspect_err(|error| {
                            let _ = tx.send(TurnEvent::Neutral(
                                agent_provider_config::protocol::StreamEvent::Failed {
                                    error: error.model_error(),
                                },
                            ));
                        })
                        .map_err(|error| error.to_string())
                    } else {
                        Err("invalid provider-neutral model request".into())
                    };
                    // The loop hears the turn is over first; the marks held
                    // while it ran (round trips to the gateway) follow.
                    marks.plan_over(ticket);
                    let _ = tx.send(TurnEvent::Finished(result));
                    marks.release(ticket);
                }
            });
        if spawned.is_err() {
            marks.release(ticket);
            let _ = tx.send(TurnEvent::Finished(Err(
                "could not start the model turn".into()
            )));
        }
        rx
    }
}

mod conversation_view;
pub use conversation_view::Answered;

/// One validated model turn: its tool calls in order and the words it said
/// beside them. A turn that made no call is the model answering in words —
/// how a chat-style model ends a turn — and `calls` is empty then.
#[derive(Clone, Debug)]
pub struct Turn {
    /// The assistant turn as the model sent it, to record in the history.
    pub assistant: Value,
    pub calls: Vec<ToolCall>,
    /// The turn's own text, never its exposed reasoning (presentation only).
    pub said: String,
}

/// The words a model turn said: its text blocks, never its exposed reasoning,
/// which is presentation only.
pub(crate) fn said_text(message: &agent_provider_config::protocol::Message) -> String {
    written_text(message).trim().to_owned()
}

/// The words a model turn wrote, exactly as written: no whitespace trimmed,
/// because an answer cut off mid-sentence continues from its last character
/// and the space before the next word matters.
pub(crate) fn written_text(message: &agent_provider_config::protocol::Message) -> String {
    message
        .content
        .iter()
        .filter_map(|content| match content {
            agent_provider_config::protocol::Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Provider-neutral history for the discover → invoke → observe loop.
#[derive(Clone, Debug)]
pub struct Conversation {
    history: agent_provider_config::protocol::Conversation,
    /// Turns of the person's (directions, pictures shown) placed after the
    /// history message at each index.
    directions: Vec<(usize, agent_provider_config::protocol::Message)>,
    first: Option<FirstTurn>,
    /// Tool results put aside to keep the conversation within its model's
    /// context: what each call's result is sent as instead, by call id.
    /// The history keeps every result as it was, so call ids still validate
    /// and nothing the run did is forgotten; only what the model is sent
    /// is shorter.
    compacted: HashMap<String, String>,
    /// The history's turns before this message index folded into a note
    /// closing the first user message: a long run's oldest turns, each
    /// result kept for reading again ([`Self::fold`]).
    folded: Option<(usize, String)>,
    /// The most tokens one answer may take: the selected model's declared
    /// output limit ([`agent_provider_config::ModelDescriptor::output_tokens`]).
    output_tokens: u32,
    /// The part of an answer already written when the model cut it off at
    /// its output limit, until the continuation that goes on from it.
    resume: Option<String>,
    #[cfg(test)]
    legacy: Option<LegacyConversation>,
}

impl Conversation {
    /// Compatibility constructor; provider selection belongs to the gateway.
    /// A legacy test fixture keeps the provider's own dialect it is scripted
    /// for; production always speaks the neutral protocol.
    pub fn new(
        protocol: agent_provider_config::ProviderProtocol,
        model: &str,
        system: &str,
        first: &FirstTurn,
    ) -> Self {
        let _ = (protocol, model);
        let conversation = Self::neutral_first(system, first);
        #[cfg(test)]
        let conversation = Self {
            legacy: Some(LegacyConversation::new(protocol, model, system, first)),
            ..conversation
        };
        conversation
    }

    pub fn neutral_first(system: &str, first: &FirstTurn) -> Self {
        Self {
            first: Some(first.clone()),
            ..Self::neutral(system, &first.text())
        }
    }

    pub fn neutral(system: &str, user_turn: &str) -> Self {
        Self {
            history: agent_provider_config::protocol::Conversation::new(system, user_turn),
            directions: Vec::new(),
            first: None,
            compacted: HashMap::new(),
            folded: None,
            output_tokens: agent_provider_config::ModelDescriptor::DEFAULT_OUTPUT_TOKENS,
            resume: None,
            #[cfg(test)]
            legacy: None,
        }
    }

    /// This conversation asking `model` for its turns' answers: one answer may
    /// take the model's own declared output limit, never a constant.
    #[must_use]
    pub fn budgeted_by(mut self, model: &agent_provider_config::ModelDescriptor) -> Self {
        self.output_tokens = model.output_tokens();
        self
    }

    /// The most tokens one answer this conversation asks for may take.
    #[must_use]
    pub const fn output_tokens(&self) -> u32 {
        self.output_tokens
    }

    /// The part of an answer already written when the model cut it off at
    /// its output limit, until the continuation that goes on from it.
    #[cfg(test)]
    pub const fn continuing(&self) -> Option<&String> {
        self.resume.as_ref()
    }

    /// Continue an answer the model cut off at its output limit: the next
    /// request carries `partial` after what it already wrote, with the
    /// instruction to go on from where it stopped. The history itself is
    /// untouched — a cut-off turn ran no call.
    pub fn continue_from(&mut self, partial: &str) {
        let written = self.resume.take().unwrap_or_default();
        self.resume = Some(format!("{written}{partial}"));
    }

    /// The cut-off answer was continued and the turn finished: the next
    /// requests are the history alone again.
    pub fn continued(&mut self) {
        self.resume = None;
    }

    pub fn streams(&self) -> bool {
        #[cfg(test)]
        if let Some(legacy) = &self.legacy {
            return legacy.streams();
        }
        true
    }

    pub fn body(&self, tools: &[agent_provider_config::protocol::ToolDefinition]) -> Value {
        use agent_provider_config::protocol::{Content, ModelRequest, ToolChoice};
        #[cfg(test)]
        if let Some(legacy) = &self.legacy {
            let mut body = legacy.body(tools, &self.compacted, self.output_tokens);
            if let Some(partial) = &self.resume
                && let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut)
            {
                // The provider's own dialect: the cut-off answer as the
                // assistant turn it was, then the instruction to go on.
                messages.push(serde_json::json!({ "role": "assistant", "content": partial }));
                messages.push(serde_json::json!({
                    "role": "user",
                    "content": "Your previous answer was cut off at the model's output limit before \
                                it finished. Continue exactly from where it stopped. Do not repeat \
                                what you already wrote, and do not perform any action you already \
                                performed.",
                }));
            }
            return body;
        }
        // Only what is sent is built: a long run's history keeps every turn
        // it ever took (for read-back and for validating call ids), while the
        // messages sent are the ones the fold left, so a turn costs what it
        // sends however many turns came before it.
        let (messages, said) = self.sent_messages();
        let mut request = ModelRequest {
            messages,
            tool_choice: if tools.is_empty() {
                ToolChoice::None
            } else {
                ToolChoice::Required
            },
            tools: tools.to_vec(),
            max_output_tokens: self.output_tokens,
        };
        if let Some(first) = &self.first {
            first.apply_to(&mut request);
        }
        if let Some((_, note)) = &self.folded
            && let Some(first) = request.messages.get_mut(1)
        {
            first.content.push(Content::Text { text: note.clone() });
            if !said.is_empty() {
                first.content.push(Content::Text {
                    text: format!(
                        "What the person said during those earlier steps:\n{}",
                        said.join("\n")
                    ),
                });
            }
        }
        serde_json::json!(self.with_continuation(request))
    }

    /// A request whose history is settled: the answer the model cut off and
    /// the instruction to go on from it are sent after it, so the one it
    /// writes next completes the answer instead of starting over.
    fn with_continuation(
        &self,
        mut request: agent_provider_config::protocol::ModelRequest,
    ) -> agent_provider_config::protocol::ModelRequest {
        let Some(partial) = &self.resume else {
            return request;
        };
        request
            .messages
            .push(agent_provider_config::protocol::Message::text(
                agent_provider_config::protocol::Role::Assistant,
                partial,
            ));
        request
            .messages
            .push(agent_provider_config::protocol::Message::text(
                agent_provider_config::protocol::Role::User,
                "Your previous answer was cut off at the model's output limit before it finished. \
                 Continue exactly from where it stopped. Do not repeat what you already wrote, and \
                 do not perform any action you already performed.",
            ));
        request
    }

    /// The history's messages as this conversation sends them: the first
    /// turn, the person's words and pictures at the turns they said them at,
    /// and every turn after the fold — each result put aside as what it was
    /// replaced with. The turns the fold covers are left out, and what the
    /// person said among them comes back for the note that replaces them.
    fn sent_messages(&self) -> (Vec<agent_provider_config::protocol::Message>, Vec<String>) {
        use agent_provider_config::protocol::{Content, Message};
        let history = self.history.messages();
        let through = self.folded_through();
        let folded = |index: usize| (2..through).contains(&index);
        let mut sent: Vec<Message> = Vec::new();
        let mut said: Vec<String> = Vec::new();
        let mut directions = self.directions.iter().peekable();
        let mut place = |index: usize, message: &Message, sent: &mut Vec<Message>| {
            if !folded(index) {
                sent.push(message.clone());
                return;
            }
            said.extend(message.content.iter().filter_map(|content| match content {
                Content::Text { text } => Some(text.clone()),
                _ => None,
            }));
        };
        for (index, message) in history.iter().enumerate() {
            while let Some((at, direction)) = directions.next_if(|(at, _)| *at == index) {
                place(*at, direction, &mut sent);
            }
            if folded(index) {
                continue;
            }
            let mut message = message.clone();
            for content in &mut message.content {
                if let Content::ToolResult(result) = content
                    && let Some(aside) = self.compacted.get(&result.call_id)
                {
                    result.content.clone_from(aside);
                }
            }
            sent.push(message);
        }
        for (at, direction) in directions {
            place((*at).min(history.len()), direction, &mut sent);
        }
        (sent, said)
    }

    /// The history index of each turn's assistant message after the first
    /// user message, oldest first: where a fold may end.
    pub fn turn_starts(&self) -> Vec<usize> {
        #[cfg(test)]
        if self.legacy.is_some() {
            return Vec::new();
        }
        self.history
            .messages()
            .iter()
            .enumerate()
            .skip(2)
            .filter(|(_, message)| message.role == agent_provider_config::protocol::Role::Assistant)
            .map(|(index, _)| index)
            .collect()
    }

    /// Send the history's turns before message `through` (a turn start) as
    /// `note` instead, at the close of the first user message. The history
    /// itself keeps them.
    pub fn fold(&mut self, through: usize, note: String) {
        self.folded = Some((through, note));
    }

    /// Where the fold ends: history messages before it are folded.
    pub fn folded_through(&self) -> usize {
        self.folded.as_ref().map_or(2, |(through, _)| *through)
    }

    /// `body`, a new turn the model just answered, with every call id this
    /// conversation already holds — or one the turn names twice — made its
    /// own, so the turn can be read and answered.
    ///
    /// A call id is the provider's, and not every provider keeps them unique
    /// across a conversation: one names its calls `call_<n>_<i>` from a few
    /// hundred numbers, so a long run meets an id an earlier turn used. Such
    /// a turn is new inference, not a replay of the earlier one (each answer
    /// is read once, as it arrives), so its call keeps everything it said and
    /// takes an id of its own; the history records it, and its result,
    /// under that id. The provider reads its calls back as this conversation
    /// records them, so the new id is the one it answers to. A body this
    /// conversation cannot read is returned as it came, for [`Self::turn`] to
    /// refuse with the reason.
    #[must_use]
    pub fn with_own_call_ids(&self, body: &str) -> String {
        #[cfg(test)]
        if self.legacy.is_some() {
            return body.to_owned();
        }
        let Ok(mut response) =
            serde_json::from_str::<agent_provider_config::protocol::ModelResponse>(body)
        else {
            return body.to_owned();
        };
        let mut taken: std::collections::BTreeSet<String> = self
            .history
            .messages()
            .iter()
            .flat_map(|message| &message.content)
            .filter_map(|content| match content {
                agent_provider_config::protocol::Content::ToolCall(call) => Some(call.id.clone()),
                _ => None,
            })
            .collect();
        let mut renamed = false;
        for content in &mut response.message.content {
            let agent_provider_config::protocol::Content::ToolCall(call) = content else {
                continue;
            };
            if call.id.is_empty() || taken.insert(call.id.clone()) {
                continue;
            }
            // Each id taken rules out at most one candidate, so one more candidate than there
            // are ids taken always holds a free one.
            let Some(own) = (2..=taken.len() + 2)
                .map(|n| format!("{}~{n}", call.id))
                .find(|id| !taken.contains(id))
            else {
                continue;
            };
            tracing::info!(
                provider_id = %call.id,
                id = %own,
                "the provider reused a call id this conversation holds; the call takes its own"
            );
            taken.insert(own.clone());
            call.id = own;
            renamed = true;
        }
        if !renamed {
            return body.to_owned();
        }
        serde_json::to_string(&response).unwrap_or_else(|_| body.to_owned())
    }

    /// The calls a turn asked for, parsed and structurally valid, before they
    /// are checked against the tools' own declarations: what
    /// [`Self::calls`] refuses for its arguments can still be answered as an
    /// error result and corrected.
    ///
    /// # Errors
    ///
    /// A body that is not a model response this conversation can read, or a
    /// turn the conversation may not replay (a consumed call id, a refusal, a
    /// truncated turn).
    pub fn read_calls(&self, body: &str) -> Result<Vec<ToolCall>, String> {
        #[cfg(test)]
        if let Some(legacy) = &self.legacy {
            return legacy.read_calls(body);
        }
        let response: agent_provider_config::protocol::ModelResponse =
            serde_json::from_str(body).map_err(|_| "invalid gateway completion".to_string())?;
        Ok(self
            .history
            .validate_response(&response)
            .map_err(|error| error.to_string())?
            .into_iter()
            .cloned()
            .collect())
    }

    /// Validate the complete turn before any of its calls may execute: its
    /// calls in order and the words it said.
    ///
    /// A chat-style model ends a turn by answering in words, calling nothing;
    /// that answer is the turn's own, never an unusable response.
    ///
    /// # Errors
    ///
    /// A body that is not a model response this conversation can read, a turn
    /// the conversation may not replay (a consumed call id, a refusal, a
    /// truncated turn), a call the loop refuses for its shape or arguments,
    /// or a turn that neither called a tool nor said anything.
    pub fn turn(&self, body: &str) -> Result<Turn, String> {
        #[cfg(test)]
        if let Some(legacy) = &self.legacy {
            return legacy.turn(body);
        }
        let response: agent_provider_config::protocol::ModelResponse =
            serde_json::from_str(body).map_err(|_| "invalid gateway completion".to_string())?;
        if response.stop_reason == agent_provider_config::protocol::StopReason::OutputLimit {
            return Err("the model's answer was cut off".into());
        }
        let calls = self
            .history
            .validate_response(&response)
            .map_err(|error| error.to_string())?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let said = match &self.resume {
            Some(written) => format!("{written}{}", said_text(&response.message)),
            None => said_text(&response.message),
        };
        if calls.is_empty() && said.is_empty() {
            return Err("the model called no tool and said nothing".into());
        }
        if !calls.is_empty() {
            tools::validate_loop_calls(&calls)?;
        }
        let mut response = response;
        if self.resume.is_some() {
            // The history records the answer as the model wrote it whole: the
            // text it stopped at, then the continuation, as one message,
            // before any call of this turn.
            response.message.content.retain(|content| {
                !matches!(
                    content,
                    agent_provider_config::protocol::Content::Text { .. }
                )
            });
            let at = response
                .message
                .content
                .iter()
                .position(|content| {
                    matches!(
                        content,
                        agent_provider_config::protocol::Content::ToolCall(_)
                    )
                })
                .unwrap_or(0);
            response.message.content.insert(
                at,
                agent_provider_config::protocol::Content::Text { text: said.clone() },
            );
        }
        Ok(Turn {
            assistant: serde_json::json!(response),
            calls,
            said,
        })
    }

    /// Answer a turn whose own calls the loop refused (a call missing an
    /// argument its declaration requires, one naming a tool that is not
    /// offered) with an error result per call, so the model reads what was
    /// wrong and corrects itself on its next turn instead of the run ending.
    /// The turn is recorded as the model sent it, exactly as [`Self::push`]
    /// records one that ran, so its call ids stay consumed and cannot replay.
    ///
    /// # Errors
    ///
    /// A body that is not a model response this conversation can read, or an
    /// assistant turn with no call to answer.
    pub fn refuse(&mut self, body: &str, refused: &[(String, String)]) -> Result<(), String> {
        #[cfg(test)]
        if let Some(legacy) = &mut self.legacy {
            let (assistant, _) = legacy.calls_unchecked(body)?;
            legacy.push(assistant, refused.to_vec());
            return Ok(());
        }
        let response: agent_provider_config::protocol::ModelResponse =
            serde_json::from_str(body).map_err(|_| "invalid gateway completion".to_string())?;
        if response.message.content.iter().all(|content| {
            !matches!(
                content,
                agent_provider_config::protocol::Content::ToolCall(_)
            )
        }) {
            return Err("the model called no tool".into());
        }
        let results = refused
            .iter()
            .map(
                |(call_id, content)| agent_provider_config::protocol::ToolResult {
                    call_id: call_id.clone(),
                    content: content.clone(),
                    is_error: true,
                },
            )
            .collect::<Vec<_>>();
        self.history
            .push(response, results)
            .map_err(|error| error.to_string())
    }

    /// Append a validated response and its ordered results atomically.
    pub fn push(&mut self, assistant: Value, results: Vec<(String, String)>) -> Result<(), String> {
        #[cfg(test)]
        if let Some(legacy) = &mut self.legacy {
            legacy.push(assistant, results);
            return Ok(());
        }
        let response = serde_json::from_value(assistant)
            .map_err(|_| "invalid gateway completion".to_string())?;
        let results = results
            .into_iter()
            .map(
                |(call_id, content)| agent_provider_config::protocol::ToolResult {
                    call_id,
                    content,
                    is_error: false,
                },
            )
            .collect();
        self.history
            .push(response, results)
            .map_err(|error| error.to_string())
    }

    pub fn push_user(&mut self, text: &str) {
        #[cfg(test)]
        if let Some(legacy) = &mut self.legacy {
            legacy.push_user(text);
            return;
        }
        self.directions.push((
            self.history.messages().len(),
            agent_provider_config::protocol::Message::text(
                agent_provider_config::protocol::Role::User,
                text,
            ),
        ));
    }

    /// Show the model `pictures` (media type, bytes), introduced by `note`,
    /// after the results that answered with them.
    pub fn push_pictures(&mut self, note: &str, pictures: Vec<(String, Vec<u8>)>) {
        use agent_provider_config::protocol::{Content, Message, Role};
        use base64::Engine as _;
        let mut content = vec![Content::Text { text: note.into() }];
        content.extend(
            pictures
                .into_iter()
                .map(|(media_type, bytes)| Content::Image {
                    media_type,
                    base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                }),
        );
        self.directions.push((
            self.history.messages().len(),
            Message {
                role: Role::User,
                content,
            },
        ));
    }

    pub fn len(&self) -> usize {
        self.history.messages().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One conversation of the discover → invoke → observe loop, in the
/// provider's own dialect.
#[cfg(test)]
#[derive(Clone, Debug)]
struct LegacyConversation {
    dialect: Dialect,
    model: String,
    system: String,
    messages: Vec<Value>,
}

#[cfg(test)]
#[allow(
    dead_code,
    reason = "the legacy provider-dialect codec a scripted test fixture carries"
)]
impl LegacyConversation {
    pub fn new(
        protocol: agent_provider_config::ProviderProtocol,
        model: &str,
        system: &str,
        first: &FirstTurn,
    ) -> Self {
        let dialect = Dialect::of(protocol);
        let messages = match dialect {
            Dialect::Anthropic => vec![first.message(dialect)],
            Dialect::OpenAi => vec![
                serde_json::json!({ "role": "system", "content": system }),
                first.message(dialect),
            ],
        };
        Self {
            dialect,
            model: model.to_owned(),
            system: system.to_owned(),
            messages,
        }
    }

    /// Whether the scripted legacy transport streams argument previews.
    pub fn streams(&self) -> bool {
        self.dialect == Dialect::OpenAi
    }

    /// The request for the next turn, offering `tools` (Anthropic-shaped
    /// definitions, re-enveloped for the `OpenAI` dialect), each result in
    /// `compacted` sent as what it maps to.
    pub fn body(
        &self,
        tools: &[agent_provider_config::protocol::ToolDefinition],
        compacted: &HashMap<String, String>,
        output_tokens: u32,
    ) -> Value {
        let tools = serde_json::json!(tools);
        let mut messages = self.messages.clone();
        let put_aside = |result: &mut Value| {
            let id = result
                .get("tool_call_id")
                .or_else(|| result.get("tool_use_id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(aside) = id.and_then(|id| compacted.get(&id))
                && let Some(object) = result.as_object_mut()
            {
                object.insert("content".into(), Value::String(aside.clone()));
            }
        };
        for message in &mut messages {
            if message.get("tool_call_id").is_some() {
                put_aside(message);
            } else if let Some(Value::Array(blocks)) = message.get_mut("content") {
                for block in blocks.iter_mut() {
                    put_aside(block);
                }
            }
        }
        let messages = &messages;
        match self.dialect {
            Dialect::Anthropic => serde_json::json!({
                "model": self.model,
                "max_tokens": output_tokens,
                "system": self.system,
                "tools": tools,
                "messages": messages,
            }),
            Dialect::OpenAi => serde_json::json!({
                "model": self.model,
                "max_completion_tokens": output_tokens,
                "messages": messages,
                "tools": openai_tools(&tools),
                "tool_choice": "required",
            }),
        }
    }

    /// The validated turn as the dialect reads it: its calls and the words it
    /// said, or the words it answered in when it called nothing.
    pub fn turn(&self, body: &str) -> Result<Turn, String> {
        let (assistant, calls) = match self.dialect {
            Dialect::Anthropic => anthropic_calls(body)?,
            Dialect::OpenAi => openai_calls(body)?,
        };
        let said = match assistant.get("content") {
            Some(Value::String(said)) => said.trim().to_owned(),
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter_map(|block| {
                    (block.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| block.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_owned(),
            _ => String::new(),
        };
        if calls.is_empty() && said.is_empty() {
            return Err("the model called no tool and said nothing".into());
        }
        if !calls.is_empty() {
            tools::validate_loop_calls(&calls)?;
        }
        Ok(Turn {
            assistant,
            calls,
            said,
        })
    }

    /// The assistant turn and its calls as the dialect reads them, before the
    /// loop's own argument checks: what [`Conversation::refuse`] answers when
    /// those checks are what refused the turn.
    pub fn calls_unchecked(&self, body: &str) -> Result<(Value, Vec<ToolCall>), String> {
        let (assistant, calls) = match self.dialect {
            Dialect::Anthropic => anthropic_calls(body)?,
            Dialect::OpenAi => openai_calls(body)?,
        };
        if calls.is_empty() {
            return Err("the model called no tool".into());
        }
        Ok((assistant, calls))
    }

    /// The calls as the dialect reads them, before the loop's own checks.
    pub fn read_calls(&self, body: &str) -> Result<Vec<ToolCall>, String> {
        self.calls_unchecked(body).map(|(_, calls)| calls)
    }

    /// Record the assistant turn and one result per call (by call id).
    pub fn push(&mut self, assistant: Value, results: Vec<(String, String)>) {
        self.messages.push(assistant);
        for (id, answer) in results {
            self.messages.push(self.dialect.tool_result(&id, &answer));
        }
        if self.dialect == Dialect::Anthropic {
            fold_anthropic_results(&mut self.messages);
        }
    }

    pub fn push_user(&mut self, text: &str) {
        self.messages
            .push(serde_json::json!({"role":"user", "content":text}));
    }
}

/// Adapt neutral deltas to the frames a turn's preview reads
/// ([`TurnEvent::Frame`]). Providers may supply the name only on the first
/// delta; retain it for subsequent chunks.
#[derive(Default)]
struct NeutralPreviews {
    names: BTreeMap<u32, String>,
}

impl NeutralPreviews {
    fn frame(&mut self, event: &agent_provider_config::protocol::StreamEvent) -> Option<String> {
        let agent_provider_config::protocol::StreamEvent::ToolCallDelta {
            index,
            id,
            name,
            arguments,
        } = event
        else {
            return None;
        };
        if let Some(name) = name {
            self.names.insert(*index, name.clone());
        }
        Some(
            serde_json::json!({
                "index": index,
                "id": id,
                "name": self.names.get(index),
                "delta": arguments,
            })
            .to_string(),
        )
    }
}

#[cfg(test)]
#[path = "model/tests.rs"]
mod tests;

/// Decode only the launcher's explicit nonsecret profile/model/protocol tuple,
/// and whether the model sees pictures. Endpoint, bearer credentials and
/// vault references never enter this contract.
pub fn managed_provider_selection(value: &Value) -> Result<ProviderSelection, String> {
    let object = value
        .as_object()
        .ok_or("managed provider must be an object")?;
    let flag = |name: &str, absent: bool| match object.get(name) {
        None => Ok(absent),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err("invalid managed provider field".to_string()),
    };
    let vision = flag("vision", false)?;
    // Said only of a model that calls no tools, so a run refuses it by name.
    let tool_calling = flag("tool_calling", true)?;
    let context_window = match object.get("context_window") {
        None => None,
        Some(Value::Number(tokens)) => Some(
            tokens
                .as_u64()
                .and_then(|tokens| u32::try_from(tokens).ok())
                .filter(|tokens| *tokens > 0)
                .ok_or("invalid managed provider field")?,
        ),
        Some(_) => return Err("invalid managed provider field".into()),
    };
    let optional = ["vision", "tool_calling", "context_window"]
        .iter()
        .filter(|key| object.contains_key(**key))
        .count();
    if object.len() != 3 + optional
        || !object.keys().all(|key| {
            matches!(
                key.as_str(),
                "profile_id"
                    | "model_id"
                    | "protocol"
                    | "vision"
                    | "tool_calling"
                    | "context_window"
            )
        })
    {
        return Err("unsupported managed provider fields".into());
    }
    let field = |name: &str| -> Result<String, String> {
        let value = object
            .get(name)
            .and_then(Value::as_str)
            .ok_or("missing managed provider field")?;
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err("invalid managed provider field".into());
        }
        Ok(value.into())
    };
    let protocol = match field("protocol")?.as_str() {
        "anthropic" => agent_provider_config::ProviderProtocol::Anthropic,
        "open_ai_compatible" => agent_provider_config::ProviderProtocol::OpenAiCompatible,
        _ => return Err("unsupported managed provider protocol".into()),
    };
    let profile_id = field("profile_id")?;
    Ok(ProviderSelection {
        profile_label: profile_id.clone(),
        profile_id,
        endpoint: String::new(),
        protocol,
        model: agent_provider_config::ModelDescriptor {
            vision,
            tool_calling,
            context_window,
            ..agent_provider_config::ModelDescriptor::new(field("model_id")?)
        },
        credential: None,
    })
}

#[cfg(test)]
mod managed_tests {
    use super::*;
    #[test]
    fn managed_selection_requires_explicit_nonsecret_gateway_binding() {
        let value = serde_json::json!({"profile_id":"profile-1","model_id":"claude-1","protocol":"anthropic"});
        let selected = managed_provider_selection(&value).unwrap();
        assert_eq!(selected.profile_id, "profile-1");
        assert_eq!(selected.model.id, "claude-1");
        assert!(selected.credential.is_none());
        assert!(selected.endpoint.is_empty());
        assert!(!selected.model.vision);
        let mut seeing = value.clone();
        seeing["vision"] = serde_json::json!(true);
        assert!(managed_provider_selection(&seeing).unwrap().model.vision);
        seeing["vision"] = serde_json::json!("yes");
        assert!(managed_provider_selection(&seeing).is_err());
        for extra in ["api_key", "endpoint", "credential"] {
            let mut changed = value.clone();
            changed[extra] = serde_json::json!("disallowed");
            assert!(managed_provider_selection(&changed).is_err());
        }
        let mut missing = value;
        missing.as_object_mut().unwrap().remove("model_id");
        assert!(managed_provider_selection(&missing).is_err());
    }

    /// The desktop says when a model calls no tools and how much it reads at
    /// once; a run reads both off the selection it was handed.
    #[test]
    fn managed_selection_carries_tool_calling_and_context() {
        let value =
            serde_json::json!({"profile_id":"p","model_id":"m","protocol":"open_ai_compatible"});
        let plain = managed_provider_selection(&value).unwrap();
        assert!(
            plain.model.tool_calling,
            "a model calls tools unless said otherwise"
        );
        assert_eq!(plain.model.context_window, None);
        let mut said = value.clone();
        said["tool_calling"] = serde_json::json!(false);
        said["context_window"] = serde_json::json!(32_768);
        let said = managed_provider_selection(&said).unwrap();
        assert!(!said.model.tool_calling);
        assert_eq!(said.model.context_window, Some(32_768));
        for bad in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!("big"),
        ] {
            let mut wrong = value.clone();
            wrong["context_window"] = bad;
            assert!(managed_provider_selection(&wrong).is_err());
        }
        let chooser = Chooser::with_provider(true, Duration::from_secs(1), said);
        let refusal = chooser.refuses_tasks().expect("refused");
        assert!(refusal.contains("m from p cannot call tools"), "{refusal}");
        assert_eq!(chooser.context_tokens(), Some(32_768));
    }
}
