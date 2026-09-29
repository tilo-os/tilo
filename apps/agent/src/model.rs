//! Talking to the model: which published operations answer a request.
//!
//! The agent never composes a document mutation of its own. It is shown the
//! live, permission-filtered catalogue of operations the room's applications
//! publish — the same catalogue a person reaches from the palette — and it
//! discovers, invokes and observes through a small set of generic tools
//! ([`crate::tools`]). Every operation id it names is checked against what a
//! tool actually showed it and against the catalogue again at dispatch; the
//! broker is the only authority.
//!
//! Two shapes of conversation live here:
//!
//! * [`Chooser::choose_actions`] — discovery, then one `plan` naming the
//!   steps (the local document and Chat runners).
//! * [`TurnRequest`] / [`TurnEvent`] — one streamed model turn of the
//!   discover → invoke → observe loop that enrolled tasks run
//!   (`session::agent_loop`). The turn runs on its own thread so completed
//!   tool calls can be executed while the model is still writing the next.
//!
//! Every call is bounded by a timeout; a slow, failed or malformed answer is
//! reported as such rather than guessed at.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use ui::time::Duration;

use serde_json::Value;

use crate::plan::Block;
use crate::providers::ProviderSelection;
use crate::surface::Catalogue;
use crate::tools::{self, Revealed};

/// The model that makes the choice.
pub const MODEL: &str = "claude-opus-5";

/// How much of the document the model is shown. A brief is small; the caps
/// keep a pathological document from making the call slow or expensive.
const MAX_BLOCKS: usize = 60;
const MAX_BLOCK_CHARS: usize = 200;

/// Model turns one action decision may take (discovery calls plus `plan`).
const MAX_ACTION_TURNS: usize = 12;
/// The most one model turn may take out of the decision's total budget, so
/// one stalled call still leaves room for another turn.
const MAX_TURN_TIME: Duration = Duration::from_secs(20);
/// The system prompt of the action-discovery conversation, in both dialects.
const ACTION_SYSTEM: &str = "Drive Tilo exclusively through the resource/action tools. Discover before planning. Preserve every argument's declared JSON type. Never invent an operation id or parameter.";

/// Which path reached the decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// A model chose, and its answer was in the exported catalogue.
    Model {
        /// The model's own one-line reason. For the log, never for the screen.
        why: String,
    },
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
    /// The answer was not an id the app exports. Refused, not acted on.
    OutOfCatalogue(String),
}

impl Route {
    /// One word for the logs: which path decided.
    pub fn label(&self) -> &'static str {
        match self {
            Route::Model { .. } => "model",
            Route::Keywords(_) => "keywords",
        }
    }

    /// Whether a model actually chose. This is the bit that must never be
    /// misrepresented: a recording made on the fallback has to be findable
    /// afterwards.
    pub fn is_live(&self) -> bool {
        matches!(self, Route::Model { .. })
    }

    /// The detail behind the label, for the log line.
    pub fn detail(&self) -> String {
        match self {
            Route::Model { why } => format!("{MODEL}: {why}"),
            Route::Keywords(Fallback::NoKey) => "no api key".to_string(),
            Route::Keywords(Fallback::Disabled) => "switched off".to_string(),
            Route::Keywords(Fallback::Failed(e)) => format!("the call failed: {e}"),
            Route::Keywords(Fallback::OutOfCatalogue(a)) => {
                format!("answered {a:?}, which the app does not export")
            }
        }
    }
}

/// Selects the gateway-held profile, then answers instructions.
pub struct Chooser {
    /// A Settings-selected provider. Its credential remains in the gateway.
    provider: Option<ProviderConnection>,
    timeout: Duration,
    enabled: bool,
    /// The content-free correlation id of the latest action decision, for
    /// the gateway's timing log (0 before the first one).
    trace: AtomicU64,
    /// Where streamed loop turns go: the gateway (set with a provider), or a
    /// scripted transport in tests.
    turns: Option<Arc<dyn TurnTransport>>,
    /// Timing marks wait here while a streamed turn owns the gateway channel.
    marks: Arc<MarkGate>,
}

struct ProviderConnection {
    selection: ProviderSelection,
}

impl Chooser {
    /// `enabled` false forces the keyword vocabulary — for a run that must be
    /// reproducible, and for proving the fallback works.
    pub fn new(enabled: bool, timeout: Duration) -> Self {
        Self {
            provider: None,
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
        let marks = Arc::<MarkGate>::default();
        let turns: Arc<dyn TurnTransport> = Arc::new(GatewayTurns {
            profile_id: selection.profile_id.clone(),
            model_id: selection.model.id.clone(),
            protocol: selection.protocol,
            marks: Arc::clone(&marks),
        });
        Self {
            provider: Some(ProviderConnection { selection }),
            timeout,
            enabled,
            trace: AtomicU64::new(0),
            turns: Some(turns),
            marks,
        }
    }

    /// A chooser whose loop turns are answered by `turns` instead of the
    /// gateway, speaking `protocol` (tests and offline proofs).
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
            timeout,
            enabled: true,
            trace: AtomicU64::new(0),
            turns: Some(turns),
            marks: Arc::default(),
        }
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
    /// ending with exactly one [`TurnEvent::Finished`].
    pub fn start_turn(
        &self,
        body: Value,
        timeout: Duration,
        turn: usize,
    ) -> std::sync::mpsc::Receiver<TurnEvent> {
        let trace = tilo_model_gateway::PlanTrace {
            id: self.trace.load(Ordering::Relaxed),
            turn: u32::try_from(turn).unwrap_or(u32::MAX),
        };
        if let Some(turns) = &self.turns {
            turns.start(TurnRequest {
                body,
                timeout,
                trace: Some(trace),
            })
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = tx.send(TurnEvent::Finished(Err("no model transport".into())));
            rx
        }
    }

    /// Whether a live call is possible at all. Cheap to ask, and worth
    /// logging at startup so an operator knows before the beat which path
    /// this run will take.
    pub fn is_live(&self) -> bool {
        self.enabled && self.provider.is_some() && tilo_model_gateway::available()
    }

    /// Decide through the live resource/action catalogue. There is no
    /// compiled-in application vocabulary: the model must
    /// discover operation ids through read-only tools, and can finish only by
    /// submitting `plan` with native JSON argument values.
    ///
    /// # Errors
    ///
    /// Returns the deterministic fallback route when model discovery is
    /// disabled, unavailable, invalid, exhausted, or misses its deadline.
    pub fn choose_actions(
        &self,
        rest: &str,
        blocks: &[Block],
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<tools::Answer, Route> {
        self.choose_actions_live(rest, blocks, catalogue, most_steps, &mut |_| {})
    }

    /// [`Self::choose_actions`], reporting what the model produces while it
    /// streams to `live` (display only; the decision is unchanged). The
    /// OpenAI-compatible dialect reports argument text as it arrives; the
    /// Anthropic dialect reports turns and tool calls.
    ///
    /// # Errors
    ///
    /// As [`Self::choose_actions`].
    pub fn choose_actions_live(
        &self,
        rest: &str,
        blocks: &[Block],
        catalogue: &Catalogue,
        most_steps: usize,
        live: &mut dyn FnMut(StreamSignal<'_>),
    ) -> Result<tools::Answer, Route> {
        let fallback = |reason| Err(Route::Keywords(reason));
        if !self.enabled {
            return fallback(Fallback::Disabled);
        }
        let answer = if let Some(provider) = &self.provider {
            if !tilo_model_gateway::available() {
                return fallback(Fallback::NoKey);
            }
            self.ask_actions_provider(provider, rest, blocks, catalogue, most_steps, live)
        } else {
            return fallback(Fallback::NoKey);
        };
        match answer {
            Ok(answer) => {
                self.mark(tilo_model_gateway::TraceEvent::PlanReady, 0);
                // Operation ids and counts only: no task text or argument values.
                tracing::info!(
                    trace = format!("{:016x}", self.trace.load(Ordering::Relaxed)),
                    actions = ?answer.actions,
                    offered = catalogue.offers.len(),
                    "model plan chosen"
                );
                Ok(answer)
            }
            Err(AskError::Failed(error)) => {
                self.mark(tilo_model_gateway::TraceEvent::PlanFailed, 0);
                fallback(Fallback::Failed(error))
            }
        }
    }

    /// Record a content-free timing mark for the latest action decision in
    /// the gateway's log (a managed agent's own stderr is discarded). Best
    /// effort and cheap: one local frame; an older gateway's refusal is
    /// ignored.
    pub fn mark(&self, event: tilo_model_gateway::TraceEvent, step: usize) {
        let id = self.trace.load(Ordering::Relaxed);
        tracing::info!(
            trace = format!("{id:016x}"),
            ?event,
            step,
            "model timing mark"
        );
        if id == 0 || self.provider.is_none() || !tilo_model_gateway::available() {
            return;
        }
        let trace = tilo_model_gateway::PlanTrace {
            id,
            turn: u32::try_from(step).unwrap_or(u32::MAX),
        };
        self.marks.mark(trace, event);
    }

    fn ask_actions_provider(
        &self,
        provider: &ProviderConnection,
        rest: &str,
        blocks: &[Block],
        catalogue: &Catalogue,
        most_steps: usize,
        live: &mut dyn FnMut(StreamSignal<'_>),
    ) -> Result<tools::Answer, AskError> {
        let dialect = Dialect::of(provider.selection.protocol);
        let began = ui::time::now();
        let remaining = || self.timeout.checked_sub(began.elapsed());
        let trace_id = uuid::Uuid::new_v4().as_u64_pair().0.max(1);
        self.trace.store(trace_id, Ordering::Relaxed);
        let mut revealed = Revealed::new();
        let mut messages = dialect.first_messages(&action_user_turn(rest, blocks));
        for turn in 0..MAX_ACTION_TURNS {
            let body = dialect.body(&provider.selection, &messages, &revealed, most_steps);
            let turn_began = ui::time::now();
            let trace = tilo_model_gateway::PlanTrace {
                id: trace_id,
                turn: u32::try_from(turn).unwrap_or(u32::MAX),
            };
            live(StreamSignal::Turn(turn));
            let raw = post_action_turn(provider, dialect, body, &remaining, trace, live);
            tracing::info!(
                trace = format!("{trace_id:016x}"),
                turn,
                protocol = ?provider.selection.protocol,
                turn_ms = turn_began.elapsed().as_millis() as u64,
                total_ms = began.elapsed().as_millis() as u64,
                ok = raw.is_ok(),
                "model turn"
            );
            match dialect.parse(&raw?).map_err(AskError::Failed)? {
                ActionTurn::Plan { input } => {
                    return tools::read_plan(&input, &revealed, most_steps)
                        .map_err(AskError::Failed);
                }
                ActionTurn::Tools { assistant, calls } => {
                    messages.push(assistant);
                    if dialect == Dialect::Anthropic {
                        // The OpenAI stream reported these as they completed.
                        for call in &calls {
                            live(StreamSignal::ToolCall(&call.name));
                        }
                    }
                    for call in calls {
                        let answer =
                            tools::answer(&call.name, &call.input, catalogue, &mut revealed);
                        messages.push(dialect.tool_result(&call.id, &answer));
                    }
                    if dialect == Dialect::Anthropic {
                        fold_anthropic_results(&mut messages);
                    }
                }
            }
        }
        Err(AskError::Failed(
            "the model exhausted the action-discovery turn limit".into(),
        ))
    }
}

#[derive(Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

enum ActionTurn {
    Tools {
        /// The assistant turn to replay, in the provider's own dialect.
        assistant: Value,
        calls: Vec<ToolCall>,
    },
    Plan {
        input: Value,
    },
}

/// The two wire dialects of the action-discovery tool loop. Both run the
/// same conversation with the same bounds: read-only discovery tools, then
/// exactly one `plan` call, alone in its turn, as the final answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    /// Anthropic Messages: `system` key, `input_schema` tools, `tool_use`
    /// blocks, `tool_result` blocks in one user turn.
    Anthropic,
    /// `OpenAI` Chat Completions: a system message, `function` tools,
    /// `message.tool_calls` with JSON-string arguments, `role: "tool"` replies.
    OpenAi,
}

impl Dialect {
    fn of(protocol: agent_provider_config::ProviderProtocol) -> Self {
        match protocol {
            agent_provider_config::ProviderProtocol::Anthropic => Self::Anthropic,
            agent_provider_config::ProviderProtocol::OpenAiCompatible => Self::OpenAi,
        }
    }

    fn first_messages(self, user_turn: &str) -> Vec<Value> {
        match self {
            Self::Anthropic => vec![serde_json::json!({ "role": "user", "content": user_turn })],
            Self::OpenAi => vec![
                serde_json::json!({ "role": "system", "content": ACTION_SYSTEM }),
                serde_json::json!({ "role": "user", "content": user_turn }),
            ],
        }
    }

    fn body(
        self,
        selection: &ProviderSelection,
        messages: &[Value],
        revealed: &Revealed,
        most_steps: usize,
    ) -> Value {
        match self {
            Self::Anthropic => action_request_body_for(selection, messages, revealed, most_steps),
            Self::OpenAi => openai_action_request_body(selection, messages, revealed, most_steps),
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

/// Anthropic carries every `tool_result` of a turn in one user message:
/// gather the trailing result blocks pushed after the assistant turn.
fn fold_anthropic_results(messages: &mut Vec<Value>) {
    let start = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .map_or(0, |index| index + 1);
    let results = messages.split_off(start);
    messages.push(serde_json::json!({ "role": "user", "content": results }));
}

fn action_request_body_for(
    selection: &ProviderSelection,
    messages: &[Value],
    revealed: &Revealed,
    most_steps: usize,
) -> Value {
    serde_json::json!({
        "model": selection.model.id,
        "max_tokens": 4096,
        "system": ACTION_SYSTEM,
        "tools": tools::definitions(revealed, most_steps),
        "messages": messages,
    })
}

/// The `OpenAI` Chat Completions request for one discovery turn. The tool
/// definitions are the same ones the Anthropic dialect sends, re-enveloped;
/// `tool_choice: "required"` keeps the model inside the tool conversation
/// (an answer that neither discovers nor plans is an error in both dialects).
/// No `reasoning_effort` is chosen here: the gateway applies the profile's.
fn openai_action_request_body(
    selection: &ProviderSelection,
    messages: &[Value],
    revealed: &Revealed,
    most_steps: usize,
) -> Value {
    serde_json::json!({
        "model": selection.model.id,
        "max_completion_tokens": 4096,
        "messages": messages,
        "tools": openai_tools(&tools::definitions(revealed, most_steps)),
        "tool_choice": "required",
    })
}

/// `{name, description, input_schema}` → `{type: "function", function:
/// {name, description, parameters}}`.
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

fn parse_action_turn(body: &str) -> Result<ActionTurn, String> {
    let (assistant, calls) = anthropic_calls(body)?;
    settle_turn(assistant, calls)
}

/// The assistant turn (to replay) and its tool calls, in call order.
fn anthropic_calls(body: &str) -> Result<(Value, Vec<ToolCall>), String> {
    let root: Value = serde_json::from_str(body)
        .map_err(|error| format!("unreadable action response: {error}"))?;
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
        });
    }
    Ok((
        serde_json::json!({ "role": "assistant", "content": content }),
        calls,
    ))
}

fn parse_openai_action_turn(body: &str) -> Result<ActionTurn, String> {
    let (assistant, calls) = openai_calls(body)?;
    settle_turn(assistant, calls)
}

/// The assistant turn (to replay) and its tool calls, in call order.
fn openai_calls(body: &str) -> Result<(Value, Vec<ToolCall>), String> {
    let root: Value = serde_json::from_str(body)
        .map_err(|error| format!("unreadable action response: {error}"))?;
    let choice = root
        .pointer("/choices/0")
        .ok_or_else(|| "action response had no choices".to_string())?;
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
        });
    }
    Ok((
        serde_json::json!({
            "role": "assistant",
            "content": message.get("content").cloned().unwrap_or(Value::Null),
            "tool_calls": tool_calls,
        }),
        calls,
    ))
}

/// The rule shared by both dialects: `plan` is the final answer and must be
/// the only call of its turn; otherwise the turn must call discovery tools.
fn settle_turn(assistant: Value, calls: Vec<ToolCall>) -> Result<ActionTurn, String> {
    let plans = calls.iter().filter(|call| call.name == tools::PLAN).count();
    if plans > 0 {
        if plans > 1 || calls.len() > 1 {
            return Err("plan must be the only tool call in its turn".into());
        }
        let input = calls.into_iter().next().map(|call| call.input);
        return Ok(ActionTurn::Plan {
            input: input.unwrap_or_else(|| serde_json::json!({})),
        });
    }
    if calls.is_empty() {
        return Err("the model neither discovered actions nor submitted a plan".into());
    }
    Ok(ActionTurn::Tools { assistant, calls })
}

fn action_user_turn(rest: &str, blocks: &[Block]) -> String {
    let mut text = String::from("The shared document as it stands:\n\n");
    for (index, block) in blocks.iter().take(MAX_BLOCKS).enumerate() {
        let _ = writeln!(
            text,
            "{}. {}: {}",
            index + 1,
            block.kind,
            first_chars(block.text.trim(), MAX_BLOCK_CHARS)
        );
    }
    let _ = write!(text, "\nThe instruction asks for:\n\n{}\n", rest.trim());
    text
}

/// Send through the profile selected for this task. The gateway resolves the
/// current profile and its vault-held key; no provider endpoint or bearer
/// value is used in this process.
fn post_provider(
    provider: &ProviderConnection,
    body: &Value,
    remaining: &dyn Fn() -> Option<Duration>,
) -> Result<String, AskError> {
    // The agent has no provider network namespace and cannot resolve a vault
    // reference. All profile lookup, credential resolution, and egress happen
    // at the per-user model gateway reached through a launcher-issued FD.
    let timeout = remaining().ok_or_else(|| AskError::Failed("the call timed out".into()))?;
    tilo_model_gateway::plan(
        &provider.selection.profile_id,
        &provider.selection.model.id,
        body.clone(),
        timeout,
    )
    .map_err(|error| AskError::Failed(error.to_string()))
}

/// Send one action-discovery turn. The `OpenAI` dialect streams: the gateway
/// reports each tool call the moment it is complete (logged here), then the
/// assembled body, so the loop itself is identical for both dialects.
fn post_action_turn(
    provider: &ProviderConnection,
    dialect: Dialect,
    body: Value,
    remaining: &dyn Fn() -> Option<Duration>,
    trace: tilo_model_gateway::PlanTrace,
    live: &mut dyn FnMut(StreamSignal<'_>),
) -> Result<String, AskError> {
    let timeout = remaining()
        .filter(|left| !left.is_zero())
        .ok_or_else(|| AskError::Failed("the call timed out".into()))?
        .min(MAX_TURN_TIME);
    if dialect == Dialect::Anthropic {
        return post_provider(provider, &body, &|| Some(timeout));
    }
    let began = ui::time::now();
    let mut scanners: BTreeMap<u64, plan_stream::PlanArgumentsScanner> = BTreeMap::new();
    tilo_model_gateway::plan_streaming_deltas(
        &provider.selection.profile_id,
        &provider.selection.model.id,
        body,
        timeout,
        Some(trace),
        &mut |event, output| {
            if event == tilo_model_gateway::EVENT_TOOL_CALL_DELTA {
                stream_plan_delta(&mut scanners, output, live);
                return;
            }
            tracing::debug!(
                event,
                turn = trace.turn,
                at_ms = began.elapsed().as_millis() as u64,
                "model tool call complete"
            );
            if event == tilo_model_gateway::EVENT_TOOL_CALL
                && let Ok(call) = serde_json::from_str::<Value>(output)
                && let Some(name) = call.get("name").and_then(Value::as_str)
            {
                live(StreamSignal::ToolCall(name));
            }
        },
    )
    .map_err(|error| AskError::Failed(error.to_string()))
}

// ---------------------------------------------------------------------------
// Streamed loop turns
// ---------------------------------------------------------------------------

/// One event of a streamed loop turn, in arrival order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnEvent {
    /// An intermediate gateway frame: `event` is
    /// [`tilo_model_gateway::EVENT_TOOL_CALL_DELTA`] or
    /// [`tilo_model_gateway::EVENT_TOOL_CALL`], `output` its JSON.
    Frame { event: String, output: String },
    /// The turn is over: the assembled provider body, or why it failed.
    Finished(Result<String, String>),
}

/// One turn to stream.
#[derive(Clone, Debug)]
pub struct TurnRequest {
    pub body: Value,
    pub timeout: Duration,
    pub trace: Option<tilo_model_gateway::PlanTrace>,
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
pub struct MarkGate {
    state: Mutex<MarkState>,
}

#[derive(Debug, Default)]
struct MarkState {
    busy: bool,
    /// Held marks with the Unix ms at which each happened.
    pending: Vec<(
        tilo_model_gateway::PlanTrace,
        tilo_model_gateway::TraceEvent,
        u64,
    )>,
}

impl MarkGate {
    fn mark(&self, trace: tilo_model_gateway::PlanTrace, event: tilo_model_gateway::TraceEvent) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.busy {
            if state.pending.len() < 256 {
                let at = ui::time::wall()
                    .duration_since(ui::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| {
                        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
                    });
                state.pending.push((trace, event, at));
            }
            return;
        }
        // Held across the call so a turn cannot start on the channel mid-mark.
        let _ = tilo_model_gateway::trace(trace, event);
    }

    fn acquire(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.busy = true;
        }
    }

    fn release(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        for (trace, event, at) in std::mem::take(&mut state.pending) {
            let _ = tilo_model_gateway::trace_at(trace, event, Some(at));
        }
        state.busy = false;
    }
}

/// Turns over the launcher-issued model gateway.
struct GatewayTurns {
    profile_id: String,
    model_id: String,
    protocol: agent_provider_config::ProviderProtocol,
    marks: Arc<MarkGate>,
}

impl TurnTransport for GatewayTurns {
    fn start(&self, request: TurnRequest) -> std::sync::mpsc::Receiver<TurnEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        let profile_id = self.profile_id.clone();
        let model_id = self.model_id.clone();
        let protocol = self.protocol;
        let marks = Arc::clone(&self.marks);
        marks.acquire();
        let spawned = std::thread::Builder::new()
            .name("agent-model-turn".into())
            .spawn({
                let tx = tx.clone();
                let marks = Arc::clone(&marks);
                move || {
                    let result = match protocol {
                        agent_provider_config::ProviderProtocol::Anthropic => {
                            tilo_model_gateway::plan(
                                &profile_id,
                                &model_id,
                                request.body,
                                request.timeout,
                            )
                        }
                        agent_provider_config::ProviderProtocol::OpenAiCompatible => {
                            tilo_model_gateway::plan_streaming_deltas(
                                &profile_id,
                                &model_id,
                                request.body,
                                request.timeout,
                                request.trace,
                                &mut |event, output| {
                                    let _ = tx.send(TurnEvent::Frame {
                                        event: event.to_owned(),
                                        output: output.to_owned(),
                                    });
                                },
                            )
                        }
                    };
                    marks.release();
                    let _ = tx.send(TurnEvent::Finished(
                        result.map_err(|error| error.to_string()),
                    ));
                }
            });
        if spawned.is_err() {
            marks.release();
            let _ = tx.send(TurnEvent::Finished(Err(
                "could not start the model turn".into()
            )));
        }
        rx
    }
}

/// One conversation of the discover → invoke → observe loop, in the
/// provider's own dialect.
#[derive(Clone, Debug)]
pub struct Conversation {
    dialect: Dialect,
    model: String,
    system: String,
    messages: Vec<Value>,
}

impl Conversation {
    pub fn new(
        protocol: agent_provider_config::ProviderProtocol,
        model: &str,
        system: &str,
        user_turn: &str,
    ) -> Self {
        let dialect = Dialect::of(protocol);
        let messages = match dialect {
            Dialect::Anthropic => vec![serde_json::json!({ "role": "user", "content": user_turn })],
            Dialect::OpenAi => vec![
                serde_json::json!({ "role": "system", "content": system }),
                serde_json::json!({ "role": "user", "content": user_turn }),
            ],
        };
        Self {
            dialect,
            model: model.to_owned(),
            system: system.to_owned(),
            messages,
        }
    }

    /// Whether the provider streams argument deltas (`OpenAI` dialect).
    pub fn streams(&self) -> bool {
        self.dialect == Dialect::OpenAi
    }

    /// The request for the next turn, offering `tools` (Anthropic-shaped
    /// definitions, re-enveloped for the `OpenAI` dialect).
    pub fn body(&self, tools: &Value) -> Value {
        match self.dialect {
            Dialect::Anthropic => serde_json::json!({
                "model": self.model,
                "max_tokens": 4096,
                "system": self.system,
                "tools": tools,
                "messages": self.messages,
            }),
            Dialect::OpenAi => serde_json::json!({
                "model": self.model,
                "max_completion_tokens": 4096,
                "messages": self.messages,
                "tools": openai_tools(tools),
                "tool_choice": "required",
            }),
        }
    }

    /// The assistant turn and its tool calls, in call order.
    ///
    /// # Errors
    ///
    /// When the body is unreadable, a refusal, or carries no tool call.
    pub fn calls(&self, body: &str) -> Result<(Value, Vec<ToolCall>), String> {
        let (assistant, calls) = match self.dialect {
            Dialect::Anthropic => anthropic_calls(body)?,
            Dialect::OpenAi => openai_calls(body)?,
        };
        if calls.is_empty() {
            return Err("the model called no tool".into());
        }
        Ok((assistant, calls))
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

    /// How many messages the conversation holds (tests and logs).
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// What the model is producing while it streams. Display only: nothing here
/// is acted on; the finished answer is still read and checked as before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamSignal<'a> {
    /// A model turn (zero-based) is starting.
    Turn(usize),
    /// A tool call completed (its name).
    ToolCall(&'a str),
    /// The `plan` call's `reply` so far.
    Reply { text: &'a str, done: bool },
    /// The `plan` call's `steps[step].operation` so far.
    StepOperation { step: usize, operation: &'a str },
    /// A string-valued `steps[step].arguments.<name>` of the `plan` call so
    /// far. Which arguments are text is the operation descriptor's to say.
    StepArgument {
        step: usize,
        name: &'a str,
        text: &'a str,
        done: bool,
    },
    /// The `operation` of the `invoke` call at index `call` of this turn,
    /// so far.
    CallOperation {
        call: usize,
        operation: &'a str,
        done: bool,
    },
    /// A string-valued `arguments.<name>` of the `invoke` call at index
    /// `call`, so far.
    CallArgument {
        call: usize,
        name: &'a str,
        text: &'a str,
        done: bool,
    },
}

/// Feed one `tool_call_delta` frame of a `plan` or `invoke` call to the
/// scanner of its call index.
pub(crate) fn stream_plan_delta(
    scanners: &mut BTreeMap<u64, plan_stream::PlanArgumentsScanner>,
    output: &str,
    live: &mut dyn FnMut(StreamSignal<'_>),
) {
    let Ok(frame) = serde_json::from_str::<Value>(output) else {
        return;
    };
    if !matches!(
        frame.get("name").and_then(Value::as_str),
        Some(tools::PLAN | tools::INVOKE)
    ) {
        return;
    }
    let (Some(index), Some(delta)) = (
        frame.get("index").and_then(Value::as_u64),
        frame.get("delta").and_then(Value::as_str),
    ) else {
        return;
    };
    scanners
        .entry(index)
        .or_default()
        .feed(delta, &mut |field, text, done| {
            live(match *field {
                plan_stream::Field::Reply => StreamSignal::Reply { text, done },
                plan_stream::Field::StepOperation(step) => StreamSignal::StepOperation {
                    step,
                    operation: text,
                },
                plan_stream::Field::StepArgument { step, ref name } => StreamSignal::StepArgument {
                    step,
                    name,
                    text,
                    done,
                },
                plan_stream::Field::Operation => StreamSignal::CallOperation {
                    call: usize::try_from(index).unwrap_or(usize::MAX),
                    operation: text,
                    done,
                },
                plan_stream::Field::Argument { ref name } => StreamSignal::CallArgument {
                    call: usize::try_from(index).unwrap_or(usize::MAX),
                    name,
                    text,
                    done,
                },
            });
        });
}

/// Why a call to the model produced nothing.
enum AskError {
    /// Transport, HTTP status, a decline, or an unreadable body.
    Failed(String),
}

// ---------------------------------------------------------------------------

fn first_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Instruction;
    use crate::surface::Offer;
    use std::collections::BTreeMap;
    use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
    use tilo_actions::{ActionContract, ActionParam, ExecutionLocus, OperationKind, ParamType};

    fn block(kind: &str, text: &str) -> Block {
        Block {
            id: "b".into(),
            kind: kind.into(),
            text: text.into(),
            attrs: BTreeMap::new(),
        }
    }

    #[test]
    fn live_tool_transcript_selects_an_arbitrary_typed_app_action() {
        let offer = Offer {
            app: "calculator".to_string(),
            broker_peer_id: None,
            descriptor: ActionDescriptor {
                address: ActionAddress::new("calculator.range:main", "sample"),
                resource_kind: "calculator.range".into(),
                resource_label: "Main range".into(),
                action_label: "Sample values".into(),
                description: Some("Generate a bounded sample".into()),
                locus: ExecutionLocus::ClientLocal,
                kind: OperationKind::Document,
                contract: ActionContract::default(),
                input: InputSpec::None,
                params: vec![
                    ActionParam::required("count", ParamType::Integer, "Number of values"),
                    ActionParam::required("normalise", ParamType::Boolean, "Normalise output"),
                ],
                view: None,
            },
        };
        let catalogue = Catalogue {
            offers: vec![offer],
            facts: Vec::new(),
            held: vec!["calculator"],
        };
        let mut revealed = Revealed::new();
        let encoded = serde_json::to_string(&tools::definitions(&revealed, 4)).unwrap();
        assert!(encoded.contains(tools::LIST_OPERATIONS));
        assert!(!encoded.contains("turn the bullets into a table"));

        let discovery = serde_json::json!({
            "stop_reason": "tool_use",
            "content": [{
                "type": "tool_use",
                "id": "tool-1",
                "name": tools::LIST_OPERATIONS,
                "input": {"application": "calculator"}
            }]
        })
        .to_string();
        let ActionTurn::Tools { calls, .. } = parse_action_turn(&discovery).unwrap() else {
            panic!("the first turn must be discovery");
        };
        let listed = tools::answer(&calls[0].name, &calls[0].input, &catalogue, &mut revealed);
        assert!(listed.contains("calculator.range:main#sample"));

        let final_turn = serde_json::json!({
            "stop_reason": "tool_use",
            "content": [{
                "type": "tool_use",
                "id": "tool-2",
                "name": tools::PLAN,
                "input": {
                    "steps": [{
                        "operation": "calculator.range:main#sample",
                        "arguments": {"count": 12, "normalise": true}
                    }],
                    "why": "the calculator publishes the requested sampler",
                    "then": null
                }
            }]
        })
        .to_string();
        let ActionTurn::Plan { input } = parse_action_turn(&final_turn).unwrap() else {
            panic!("the second turn must be the final plan");
        };
        let answer = tools::read_plan(&input, &revealed, 4).unwrap();
        let instruction = Instruction::from_answer(
            &answer.actions[0],
            "sample twelve values",
            Some(&answer.arguments[0]),
            &catalogue.offers,
        )
        .unwrap();
        assert_eq!(
            instruction,
            Instruction::Perform {
                id: "calculator.range:main#sample".into(),
                arguments: serde_json::Map::from_iter([
                    ("count".into(), serde_json::json!(12)),
                    ("normalise".into(), serde_json::json!(true)),
                ]),
                confirmation: crate::plan::ConfirmationEvidence::None,
            }
        );
    }

    #[test]
    fn openai_dialect_runs_the_same_tool_loop_with_function_tools() {
        let mut profile = agent_provider_config::ProviderProfile::hosted(
            agent_provider_config::ProviderProtocol::OpenAiCompatible,
        );
        profile
            .models
            .push(agent_provider_config::ModelDescriptor::new("gpt-5.4-nano"));
        let selection = ProviderSelection::from_profile(&profile, profile.models[0].clone());
        let dialect = Dialect::of(selection.protocol);
        assert_eq!(dialect, Dialect::OpenAi);
        let revealed = Revealed::new();
        let mut messages = dialect.first_messages("turn the bullets into a table");
        let body = dialect.body(&selection, &messages, &revealed, 4);

        // The request is Chat Completions shaped: no Anthropic keys at all.
        assert_eq!(body["model"], "gpt-5.4-nano");
        assert_eq!(body["max_completion_tokens"], 4096);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("system").is_none());
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], ACTION_SYSTEM);
        assert_eq!(body["messages"][1]["role"], "user");
        let tools = body["tools"].as_array().unwrap();
        let definitions = tools::definitions(&revealed, 4);
        assert_eq!(tools.len(), definitions.as_array().unwrap().len());
        for (tool, definition) in tools.iter().zip(definitions.as_array().unwrap()) {
            assert_eq!(tool["type"], "function");
            assert_eq!(tool["function"]["name"], definition["name"]);
            assert_eq!(tool["function"]["description"], definition["description"]);
            assert_eq!(tool["function"]["parameters"], definition["input_schema"]);
            assert!(tool.get("input_schema").is_none());
        }

        // A discovery turn: JSON-string arguments, replayed with role:"tool".
        let discovery = serde_json::json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {"id": "call_1", "type": "function", "function": {"name": tools::LIST_OPERATIONS, "arguments": "{\"application\":\"calculator\"}"}},
                        {"id": "call_2", "type": "function", "function": {"name": tools::LIST_APPLICATIONS, "arguments": ""}}
                    ]
                }
            }]
        })
        .to_string();
        let ActionTurn::Tools { assistant, calls } = dialect.parse(&discovery).unwrap() else {
            panic!("a discovery turn");
        };
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].input,
            serde_json::json!({"application": "calculator"})
        );
        assert_eq!(calls[1].input, serde_json::json!({}));
        assert_eq!(assistant["tool_calls"][1]["id"], "call_2");
        messages.push(assistant);
        for call in &calls {
            messages.push(dialect.tool_result(&call.id, "listed"));
        }
        assert_eq!(messages[3]["role"], "tool");
        assert_eq!(messages[3]["tool_call_id"], "call_1");
        assert_eq!(messages[4]["tool_call_id"], "call_2");
        assert_eq!(messages[4]["content"], "listed");

        // The plan call is the final answer.
        let plan_turn = serde_json::json!({
            "choices": [{"message": {"tool_calls": [
                {"id": "call_3", "type": "function", "function": {"name": tools::PLAN, "arguments": "{\"steps\":[],\"why\":\"x\",\"then\":null}"}}
            ]}}]
        })
        .to_string();
        let ActionTurn::Plan { input } = dialect.parse(&plan_turn).unwrap() else {
            panic!("the final plan");
        };
        assert_eq!(input["why"], "x");
    }

    /// With `TILO_AGENT_DUMP_OPENAI_BODY=<path>`, write the first `OpenAI`
    /// discovery request (real tool schemas) for a live provider check.
    #[test]
    fn openai_first_turn_body_can_be_dumped_for_a_live_check() {
        let Some(path) = std::env::var_os("TILO_AGENT_DUMP_OPENAI_BODY") else {
            return;
        };
        let mut profile = agent_provider_config::ProviderProfile::hosted(
            agent_provider_config::ProviderProtocol::OpenAiCompatible,
        );
        profile
            .models
            .push(agent_provider_config::ModelDescriptor::new("gpt-5.4-nano"));
        let selection = ProviderSelection::from_profile(&profile, profile.models[0].clone());
        let messages = Dialect::OpenAi.first_messages(&action_user_turn(
            "make the second paragraph bold",
            &[block("paragraph", "Intro"), block("paragraph", "Details")],
        ));
        let body = Dialect::OpenAi.body(&selection, &messages, &Revealed::new(), 8);
        std::fs::write(path, serde_json::to_vec_pretty(&body).unwrap()).unwrap();
    }

    #[test]
    fn openai_dialect_refuses_malformed_and_mixed_turns() {
        let turn = |calls: Value| {
            serde_json::json!({"choices": [{"message": {"tool_calls": calls}}]}).to_string()
        };
        let call = |id: &str, name: &str, arguments: &str| serde_json::json!({"id": id, "type": "function", "function": {"name": name, "arguments": arguments}});
        let mixed = turn(serde_json::json!([
            call("a", tools::LIST_OPERATIONS, "{}"),
            call("b", tools::PLAN, "{}")
        ]));
        assert!(parse_openai_action_turn(&mixed).is_err());
        let two_plans = turn(serde_json::json!([
            call("a", tools::PLAN, "{}"),
            call("b", tools::PLAN, "{}")
        ]));
        assert!(parse_openai_action_turn(&two_plans).is_err());
        let bad_json = turn(serde_json::json!([call(
            "a",
            tools::LIST_OPERATIONS,
            "{not json"
        )]));
        assert!(parse_openai_action_turn(&bad_json).is_err());
        let no_id = turn(serde_json::json!([call("", tools::LIST_OPERATIONS, "{}")]));
        assert!(parse_openai_action_turn(&no_id).is_err());
        let text_only =
            serde_json::json!({"choices": [{"message": {"content": "hello"}}]}).to_string();
        assert!(parse_openai_action_turn(&text_only).is_err());
        let refusal = serde_json::json!({"choices": [{"message": {"refusal": "no"}}]}).to_string();
        assert!(parse_openai_action_turn(&refusal).is_err());
        let filtered =
            serde_json::json!({"choices": [{"finish_reason": "content_filter", "message": {}}]})
                .to_string();
        assert!(parse_openai_action_turn(&filtered).is_err());
        assert!(parse_openai_action_turn("{}").is_err());
    }

    #[test]
    fn anthropic_dialect_keeps_its_wire_shape() {
        let profile = agent_provider_config::ProviderProfile::hosted(
            agent_provider_config::ProviderProtocol::Anthropic,
        );
        let selection = ProviderSelection::from_profile(
            &profile,
            agent_provider_config::ModelDescriptor::new("claude-x"),
        );
        let dialect = Dialect::of(selection.protocol);
        let mut messages = dialect.first_messages("do it");
        let body = dialect.body(&selection, &messages, &Revealed::new(), 4);
        assert_eq!(body["system"], ACTION_SYSTEM);
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
        assert!(body["tools"][0].get("input_schema").is_some());
        messages.push(serde_json::json!({"role": "assistant", "content": []}));
        messages.push(dialect.tool_result("t1", "a"));
        messages.push(dialect.tool_result("t2", "b"));
        fold_anthropic_results(&mut messages);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][1]["tool_use_id"], "t2");
    }
}

/// Decode only the launcher's explicit nonsecret profile/model/protocol tuple.
/// Endpoint, bearer credentials and vault references never enter this contract.
///
/// # Errors
///
/// Returns a description of the first violation when `value` is not exactly
/// that tuple or names an unknown protocol or model.
pub fn managed_provider_selection(value: &Value) -> Result<ProviderSelection, String> {
    let object = value
        .as_object()
        .ok_or("managed provider must be an object")?;
    if object.len() != 3
        || !object
            .keys()
            .all(|key| matches!(key.as_str(), "profile_id" | "model_id" | "protocol"))
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
        model: agent_provider_config::ModelDescriptor::new(field("model_id")?),
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
        for extra in ["api_key", "endpoint", "credential"] {
            let mut changed = value.clone();
            changed[extra] = serde_json::json!("disallowed");
            assert!(managed_provider_selection(&changed).is_err());
        }
        let mut missing = value;
        missing.as_object_mut().unwrap().remove("model_id");
        assert!(managed_provider_selection(&missing).is_err());
    }
}

/// Incremental reading of a `plan` or `invoke` tool call's JSON arguments
/// while the provider is still writing them: the current (possibly
/// unterminated) value of the top-level `reply`, of each
/// `steps[i].operation`, of every string-valued `steps[i].arguments.<name>`,
/// and of an `invoke` call's top-level `operation` and string-valued
/// `arguments.<name>`. Display only: the finished plan is still read
/// by [`tools::read_plan`] from the complete arguments, never from here.
pub mod plan_stream {
    /// A string value the scanner follows.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Field {
        /// The top-level `reply`.
        Reply,
        /// `steps[i].operation`.
        StepOperation(usize),
        /// A string-valued `steps[i].arguments.<name>`.
        StepArgument { step: usize, name: String },
        /// The top-level `operation` (an `invoke` call).
        Operation,
        /// A string-valued top-level `parameters.<name>` (or
        /// `arguments.<name>`) of an `invoke` call.
        Argument { name: String },
    }

    /// Longest object key kept; longer keys never name a followed field.
    const MAX_KEY_BYTES: usize = 64;

    #[derive(Debug)]
    enum Obj {
        KeyOrEnd,
        Colon,
        Value,
        CommaOrEnd,
    }

    #[derive(Debug)]
    enum Arr {
        ValueOrEnd,
        CommaOrEnd,
    }

    #[derive(Debug)]
    enum Frame {
        Object { key: Option<String>, state: Obj },
        Array { index: usize, state: Arr },
    }

    #[derive(Debug)]
    enum Escape {
        None,
        Backslash,
        Unicode { value: u16, digits: u8 },
    }

    #[derive(Debug)]
    struct Text {
        key: bool,
        field: Option<Field>,
        /// Kept only for keys and followed fields.
        buf: String,
        escape: Escape,
        /// A high surrogate waiting for its low half.
        high: Option<u16>,
        /// Bytes of `buf` already reported.
        reported: usize,
    }

    #[derive(Debug)]
    enum Lex {
        Idle,
        Text(Text),
        Scalar,
    }

    /// Feed it the argument text in arbitrary pieces (each a whole `&str`);
    /// it reports every followed string as it grows. Malformed input stops
    /// the reports; it never panics and never allocates for unfollowed text.
    #[derive(Debug)]
    pub struct PlanArgumentsScanner {
        stack: Vec<Frame>,
        lex: Lex,
        started: bool,
        failed: bool,
    }

    impl Default for PlanArgumentsScanner {
        fn default() -> Self {
            Self::new()
        }
    }

    impl PlanArgumentsScanner {
        pub fn new() -> Self {
            Self {
                stack: Vec::new(),
                lex: Lex::Idle,
                started: false,
                failed: false,
            }
        }

        /// Whether the input so far was not JSON the scanner could follow.
        pub fn failed(&self) -> bool {
            self.failed
        }

        /// Consume `chunk`. `out(field, value, done)` sees each followed
        /// string whose value grew (once per feed, with its value so far), and
        /// once more with `done` when its closing quote arrives.
        pub fn feed(&mut self, chunk: &str, out: &mut dyn FnMut(&Field, &str, bool)) {
            for c in chunk.chars() {
                if self.failed {
                    return;
                }
                self.step(c, out);
            }
            if let Lex::Text(text) = &mut self.lex
                && let Some(field) = &text.field
                && text.buf.len() > text.reported
            {
                text.reported = text.buf.len();
                out(field, &text.buf, false);
            }
        }

        fn fail(&mut self) {
            self.failed = true;
            self.lex = Lex::Idle;
        }

        fn step(&mut self, c: char, out: &mut dyn FnMut(&Field, &str, bool)) {
            match &mut self.lex {
                Lex::Text(_) => self.text_char(c, out),
                Lex::Scalar => {
                    if matches!(c, ',' | '}' | ']') || c.is_whitespace() {
                        self.lex = Lex::Idle;
                        self.structural(c);
                    } else if !(c.is_ascii_alphanumeric() || matches!(c, '-' | '+' | '.')) {
                        self.fail();
                    }
                }
                Lex::Idle => self.structural(c),
            }
        }

        /// The path of the value about to start, as a followed field.
        fn field_here(&self) -> Option<Field> {
            fn key(frame: &Frame) -> Option<&str> {
                match frame {
                    Frame::Object { key: Some(key), .. } => Some(key.as_str()),
                    _ => None,
                }
            }
            fn index(frame: &Frame) -> Option<usize> {
                match frame {
                    Frame::Array { index, .. } => Some(*index),
                    Frame::Object { .. } => None,
                }
            }
            match self.stack.as_slice() {
                [root] if key(root) == Some("reply") => Some(Field::Reply),
                [root] if key(root) == Some("operation") => Some(Field::Operation),
                [root, arguments] if matches!(key(root), Some("parameters" | "arguments")) => {
                    Some(Field::Argument {
                        name: key(arguments)?.to_owned(),
                    })
                }
                [root, steps, step] if key(root) == Some("steps") => {
                    let i = index(steps)?;
                    (key(step) == Some("operation")).then_some(Field::StepOperation(i))
                }
                [root, steps, step, arguments]
                    if key(root) == Some("steps") && key(step) == Some("arguments") =>
                {
                    Some(Field::StepArgument {
                        step: index(steps)?,
                        name: key(arguments)?.to_owned(),
                    })
                }
                _ => None,
            }
        }

        /// A value starts here: its container must be waiting for one.
        fn begin_value(&mut self) -> bool {
            match self.stack.last_mut() {
                None => {
                    let first = !self.started;
                    self.started = true;
                    first
                }
                Some(Frame::Object { state, .. }) if matches!(state, Obj::Value) => {
                    *state = Obj::CommaOrEnd;
                    true
                }
                Some(Frame::Array { state, .. }) if matches!(state, Arr::ValueOrEnd) => {
                    *state = Arr::CommaOrEnd;
                    true
                }
                Some(_) => false,
            }
        }

        fn structural(&mut self, c: char) {
            if c.is_whitespace() {
                return;
            }
            match c {
                '{' | '[' => {
                    if !self.begin_value() {
                        return self.fail();
                    }
                    self.stack.push(if c == '{' {
                        Frame::Object {
                            key: None,
                            state: Obj::KeyOrEnd,
                        }
                    } else {
                        Frame::Array {
                            index: 0,
                            state: Arr::ValueOrEnd,
                        }
                    });
                }
                '"' => {
                    let key = matches!(
                        self.stack.last(),
                        Some(Frame::Object {
                            state: Obj::KeyOrEnd,
                            ..
                        })
                    );
                    // The field is read before `begin_value` changes nothing
                    // that it depends on (only container states).
                    let field = if key { None } else { self.field_here() };
                    if !key && !self.begin_value() {
                        return self.fail();
                    }
                    self.lex = Lex::Text(Text {
                        key,
                        field,
                        buf: String::new(),
                        escape: Escape::None,
                        high: None,
                        reported: 0,
                    });
                }
                ':' => match self.stack.last_mut() {
                    Some(Frame::Object { state, .. }) if matches!(state, Obj::Colon) => {
                        *state = Obj::Value;
                    }
                    _ => self.fail(),
                },
                ',' => match self.stack.last_mut() {
                    Some(Frame::Object { key, state }) if matches!(state, Obj::CommaOrEnd) => {
                        *key = None;
                        *state = Obj::KeyOrEnd;
                    }
                    Some(Frame::Array { index, state }) if matches!(state, Arr::CommaOrEnd) => {
                        *index = index.saturating_add(1);
                        *state = Arr::ValueOrEnd;
                    }
                    _ => self.fail(),
                },
                '}' => match self.stack.last() {
                    Some(Frame::Object {
                        state: Obj::KeyOrEnd | Obj::CommaOrEnd,
                        ..
                    }) => {
                        self.stack.pop();
                    }
                    _ => self.fail(),
                },
                ']' => match self.stack.last() {
                    Some(Frame::Array {
                        state: Arr::ValueOrEnd | Arr::CommaOrEnd,
                        ..
                    }) => {
                        self.stack.pop();
                    }
                    _ => self.fail(),
                },
                c if c.is_ascii_digit() || matches!(c, '-' | 't' | 'f' | 'n') => {
                    if !self.begin_value() {
                        return self.fail();
                    }
                    self.lex = Lex::Scalar;
                }
                _ => self.fail(),
            }
        }

        #[allow(
            clippy::too_many_lines,
            reason = "one lexer state transition per escape and character class; splitting it scatters the JSON string grammar"
        )]
        fn text_char(&mut self, c: char, out: &mut dyn FnMut(&Field, &str, bool)) {
            let Lex::Text(text) = &mut self.lex else {
                return;
            };
            let keep = text.key || text.field.is_some();
            let push = |text: &mut Text, c: char| {
                if text.key && text.buf.len() >= MAX_KEY_BYTES {
                    return;
                }
                if keep {
                    text.buf.push(c);
                }
            };
            // A pending high surrogate must be followed by `\u` + low half.
            let settle_high = |text: &mut Text| {
                if text.high.take().is_some() {
                    push(text, char::REPLACEMENT_CHARACTER);
                }
            };
            match text.escape {
                Escape::None => match c {
                    '\\' => text.escape = Escape::Backslash,
                    '"' => {
                        settle_high(text);
                        let Lex::Text(done) = std::mem::replace(&mut self.lex, Lex::Idle) else {
                            return;
                        };
                        if done.key {
                            if let Some(Frame::Object { key, state }) = self.stack.last_mut() {
                                *key = Some(done.buf);
                                *state = Obj::Colon;
                            } else {
                                self.fail();
                            }
                        } else if let Some(field) = &done.field {
                            out(field, &done.buf, true);
                        }
                    }
                    c if (c as u32) < 0x20 => self.fail(),
                    c => {
                        settle_high(text);
                        push(text, c);
                    }
                },
                Escape::Backslash => {
                    text.escape = Escape::None;
                    let simple = match c {
                        '"' => Some('"'),
                        '\\' => Some('\\'),
                        '/' => Some('/'),
                        'b' => Some('\u{8}'),
                        'f' => Some('\u{c}'),
                        'n' => Some('\n'),
                        'r' => Some('\r'),
                        't' => Some('\t'),
                        'u' => {
                            text.escape = Escape::Unicode {
                                value: 0,
                                digits: 0,
                            };
                            None
                        }
                        _ => return self.fail(),
                    };
                    if let Some(simple) = simple {
                        settle_high(text);
                        push(text, simple);
                    }
                }
                Escape::Unicode { value, digits } => {
                    let Some(digit) = c.to_digit(16) else {
                        return self.fail();
                    };
                    let value = (value << 4) | digit as u16;
                    if digits < 3 {
                        text.escape = Escape::Unicode {
                            value,
                            digits: digits + 1,
                        };
                        return;
                    }
                    text.escape = Escape::None;
                    match value {
                        0xD800..=0xDBFF => {
                            settle_high(text);
                            text.high = Some(value);
                        }
                        0xDC00..=0xDFFF => match text.high.take() {
                            Some(high) => {
                                let code = 0x10000
                                    + ((u32::from(high) - 0xD800) << 10)
                                    + (u32::from(value) - 0xDC00);
                                push(text, char::from_u32(code).unwrap_or('\u{FFFD}'));
                            }
                            None => push(text, char::REPLACEMENT_CHARACTER),
                        },
                        value => {
                            settle_high(text);
                            push(text, char::from_u32(u32::from(value)).unwrap_or('\u{FFFD}'));
                        }
                    }
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Every report, in order: (field, value, done).
        fn scan(pieces: &[&str]) -> Vec<(Field, String, bool)> {
            let mut scanner = PlanArgumentsScanner::new();
            let mut seen = Vec::new();
            for piece in pieces {
                scanner.feed(piece, &mut |field, value, done| {
                    seen.push((field.clone(), value.to_owned(), done));
                });
            }
            assert!(!scanner.failed(), "{pieces:?}");
            seen
        }

        fn finals(seen: &[(Field, String, bool)]) -> Vec<(Field, String)> {
            seen.iter()
                .filter(|(_, _, done)| *done)
                .map(|(field, value, _)| (field.clone(), value.clone()))
                .collect()
        }

        /// Split `text` at pseudo-random character boundaries.
        fn random_split(text: &str, seed: u64) -> Vec<&str> {
            let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let mut pieces = Vec::new();
            let mut start = 0;
            while start < text.len() {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let want = 1 + (state >> 33) as usize % 7;
                let mut end = (start + want).min(text.len());
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                pieces.push(&text[start..end]);
                start = end;
            }
            pieces
        }

        const ARGS: &str = r#"{"reply":"Sure \u2014 writing \"it\" now\\ok","steps":[{"operation":"broker:7:word-processor.document#append-paragraph","arguments":{"text":"The ocean \ud83c\udf0a covers\nmost\tof \u00e9arth.","n":[1,2.5e3,true,null],"style":"plain"}},{"operation":"b","arguments":{"other":{"text":"not followed"},"text":"second"}}],"why":"asked","then":null}"#;

        fn expected() -> Vec<(Field, String)> {
            vec![
                (Field::Reply, "Sure \u{2014} writing \"it\" now\\ok".into()),
                (
                    Field::StepOperation(0),
                    "broker:7:word-processor.document#append-paragraph".into(),
                ),
                (
                    Field::StepArgument {
                        step: 0,
                        name: "text".into(),
                    },
                    "The ocean \u{1f30a} covers\nmost\tof \u{e9}arth.".into(),
                ),
                (
                    Field::StepArgument {
                        step: 0,
                        name: "style".into(),
                    },
                    "plain".into(),
                ),
                (Field::StepOperation(1), "b".into()),
                (
                    Field::StepArgument {
                        step: 1,
                        name: "text".into(),
                    },
                    "second".into(),
                ),
            ]
        }

        #[test]
        fn whole_arguments_yield_every_followed_field_once_done() {
            let seen = scan(&[ARGS]);
            assert_eq!(finals(&seen), expected());
            // The values agree with a real JSON parser.
            let parsed: serde_json::Value = serde_json::from_str(ARGS).unwrap();
            assert_eq!(parsed["reply"], expected()[0].1);
            assert_eq!(parsed["steps"][0]["arguments"]["text"], expected()[2].1);
        }

        #[test]
        fn random_chunk_splits_grow_monotonically_to_the_same_values() {
            for seed in 0..300 {
                let pieces = random_split(ARGS, seed);
                let seen = scan(&pieces);
                assert_eq!(finals(&seen), expected(), "seed {seed}");
                for (field, final_value) in expected() {
                    let mut last = String::new();
                    for (_, value, _) in seen.iter().filter(|(f, _, _)| *f == field) {
                        assert!(
                            value.starts_with(&last) && final_value.starts_with(value.as_str()),
                            "seed {seed}: {field:?} went {last:?} -> {value:?}"
                        );
                        last.clone_from(value);
                    }
                }
            }
        }

        #[test]
        fn a_partial_escape_is_withheld_until_complete() {
            let seen = scan(&[r#"{"reply":"a\"#, r"u00", r"e9b\ud83c", r"\udf0a"]);
            let values: Vec<&str> = seen.iter().map(|(_, v, _)| v.as_str()).collect();
            assert_eq!(values, ["a", "a\u{e9}b", "a\u{e9}b\u{1f30a}"]);
            assert!(seen.iter().all(|(_, _, done)| !done), "still unterminated");
        }

        #[test]
        fn lone_surrogates_become_replacement_characters() {
            let seen = scan(&[r#"{"reply":"x\ud83cy\udf0az\ud83c"}"#]);
            assert_eq!(
                finals(&seen),
                [(Field::Reply, "x\u{FFFD}y\u{FFFD}z\u{FFFD}".to_string())]
            );
        }

        #[test]
        fn gateway_delta_frames_of_the_plan_call_become_stream_signals() {
            use super::super::{StreamSignal, stream_plan_delta};
            let mut scanners = std::collections::BTreeMap::new();
            let mut seen = Vec::new();
            let frames = [
                ("list_operations", r#"{"search":"text"}"#),
                ("plan", r#"{"reply":"Hi"#),
                (
                    "plan",
                    r#" there","steps":[{"operation":"op","arguments":{"text":"Oc"#,
                ),
                ("plan", r#"ean"}}],"why":"w","then":null}"#),
            ];
            for (name, delta) in frames {
                let frame =
                    serde_json::json!({"index": 1, "id": "c", "name": name, "delta": delta});
                stream_plan_delta(&mut scanners, &frame.to_string(), &mut |signal| {
                    seen.push(match signal {
                        StreamSignal::Reply { text, done } => format!("reply {text:?} {done}"),
                        StreamSignal::StepOperation { step, operation } => {
                            format!("op {step} {operation:?}")
                        }
                        StreamSignal::StepArgument {
                            step,
                            name,
                            text,
                            done,
                        } => format!("{name} {step} {text:?} {done}"),
                        other => format!("{other:?}"),
                    });
                });
            }
            assert_eq!(
                seen,
                [
                    r#"reply "Hi" false"#,
                    r#"reply "Hi there" true"#,
                    r#"op 0 "op""#,
                    r#"text 0 "Oc" false"#,
                    r#"text 0 "Ocean" true"#,
                ]
            );
        }

        /// An `invoke` call's operation and string arguments are followed at
        /// the top level, so its text can be previewed while it streams.
        #[test]
        fn invoke_frames_become_call_signals() {
            use super::super::{StreamSignal, stream_plan_delta};
            let mut scanners = std::collections::BTreeMap::new();
            let mut seen = Vec::new();
            let frames = [
                (2, "invoke", r#"{"operation":"broker:7:doc#type-t"#),
                (2, "invoke", r#"ext","parameters":{"text":"Hel"#),
                (2, "invoke", r#"lo","count":3,"nested":{"text":"no"}}}"#),
                (3, "list_operations", r#"{"search":"x"}"#),
            ];
            for (index, name, delta) in frames {
                let frame =
                    serde_json::json!({"index": index, "id": "c", "name": name, "delta": delta});
                stream_plan_delta(&mut scanners, &frame.to_string(), &mut |signal| {
                    seen.push(match signal {
                        StreamSignal::CallOperation {
                            call,
                            operation,
                            done,
                        } => format!("op {call} {operation:?} {done}"),
                        StreamSignal::CallArgument {
                            call,
                            name,
                            text,
                            done,
                        } => format!("{name} {call} {text:?} {done}"),
                        other => format!("{other:?}"),
                    });
                });
            }
            assert_eq!(
                seen,
                [
                    r#"op 2 "broker:7:doc#type-t" false"#,
                    r#"op 2 "broker:7:doc#type-text" true"#,
                    r#"text 2 "Hel" false"#,
                    r#"text 2 "Hello" true"#,
                ]
            );
        }

        #[test]
        fn malformed_input_stops_reporting_without_panicking() {
            let mut scanner = PlanArgumentsScanner::new();
            let mut reports = 0;
            scanner.feed(r#"{"reply" "x"}"#, &mut |_, _, _| reports += 1);
            assert!(scanner.failed());
            assert_eq!(reports, 0);
            let mut nested = PlanArgumentsScanner::new();
            nested.feed(r#"{"reply":{"text":"x"}}"#, &mut |_, _, _| reports += 1);
            assert!(!nested.failed());
            assert_eq!(reports, 0, "only the top-level string reply is followed");
        }
    }
}
