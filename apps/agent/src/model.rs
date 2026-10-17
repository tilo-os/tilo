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
//! * [`Chooser::choose_actions`] — the published operations up front,
//!   discovery only for what they leave out, then one `plan` naming the
//!   steps (the local document and Chat runners).
//! * [`TurnRequest`] / [`TurnEvent`] — one streamed model turn of the
//!   discover → invoke → observe loop that model tasks run
//!   (`session::agent_loop`). The turn runs on its own thread; previews
//!   remain inert until the complete response and tool batch validate.
//!
//! Every call is bounded by a timeout; a slow, failed or malformed answer is
//! reported as such rather than guessed at.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use ui::time::Duration;

use serde_json::Value;

use crate::providers::ProviderSelection;
use crate::surface::Catalogue;
use crate::tools::{self, Revealed};

/// Model turns one action decision may take (discovery calls plus `plan`).
const MAX_ACTION_TURNS: usize = 12;
/// The most one model turn may take out of the decision's total budget, so
/// one stalled call still leaves room for another turn.
const MAX_TURN_TIME: Duration = Duration::from_secs(20);
/// The system prompt of the action-discovery conversation, in both dialects.
/// Constant, like the tools, so the provider's prompt cache keeps it.
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

impl Route {
    /// The detail behind the label, for the log line.
    pub fn detail(&self) -> String {
        match self {
            Route::Keywords(Fallback::NoKey) => "no api key".to_string(),
            Route::Keywords(Fallback::Disabled) => "switched off".to_string(),
            Route::Keywords(Fallback::Failed(e)) => format!("the call failed: {e}"),
        }
    }
}

/// Selects the gateway-held profile, then answers instructions.
pub struct Chooser {
    /// A Settings-selected provider. Its credential remains in the gateway.
    provider: Option<ProviderConnection>,
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
    marks: Arc<MarkGate>,
}

struct ProviderConnection {
    selection: ProviderSelection,
}

impl Chooser {
    /// The provider this chooser asks, when it has one.
    pub fn selection(&self) -> Option<&ProviderSelection> {
        self.provider.as_ref().map(|provider| &provider.selection)
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
        let marks = Arc::<MarkGate>::default();
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
            neutral: !cfg!(test),
            timeout,
            enabled: true,
            trace: AtomicU64::new(0),
            turns: Some(turns),
            marks: Arc::default(),
        }
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
    /// compiled-in application vocabulary: the first turn carries a compact
    /// summary of the published operations, so a simple request is planned
    /// in one model turn; the read-only discovery tools reach what the
    /// summary leaves out. The model can finish only by submitting `plan`
    /// with native JSON argument values, naming only ids it was shown.
    ///
    /// # Errors
    ///
    /// Returns the deterministic fallback route when model discovery is
    /// disabled, unavailable, invalid, exhausted, or misses its deadline.
    pub fn choose_actions(
        &self,
        rest: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<tools::Answer, Route> {
        self.choose_actions_live(rest, catalogue, most_steps, &mut |_| {})
    }

    /// [`Self::choose_actions`], reporting what the model produces while it
    /// streams to `live` (display only; the decision is unchanged). Both
    /// dialects report argument text as it arrives and each tool call as it
    /// completes.
    ///
    /// # Errors
    ///
    /// As [`Self::choose_actions`].
    pub fn choose_actions_live(
        &self,
        rest: &str,
        catalogue: &Catalogue,
        most_steps: usize,
        live: &mut dyn FnMut(StreamSignal<'_>),
    ) -> Result<tools::Answer, Route> {
        self.choose_actions_observed(rest, catalogue, most_steps, live, &mut |_| {})
    }

    /// Observe presentation without granting deltas authority to act.
    ///
    /// # Errors
    /// Returns the fallback route when model access or validated planning fails.
    pub fn choose_actions_observed(
        &self,
        rest: &str,
        catalogue: &Catalogue,
        most_steps: usize,
        live: &mut dyn FnMut(StreamSignal<'_>),
        observe: &mut dyn FnMut(&agent_provider_config::protocol::StreamEvent),
    ) -> Result<tools::Answer, Route> {
        let fallback = |reason| Err(Route::Keywords(reason));
        if !self.enabled {
            return fallback(Fallback::Disabled);
        }
        let answer = if let Some(provider) = &self.provider {
            if !tilo_model_gateway::available() {
                return fallback(Fallback::NoKey);
            }
            self.ask_actions_provider(provider, rest, catalogue, most_steps, live, observe)
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
        catalogue: &Catalogue,
        most_steps: usize,
        live: &mut dyn FnMut(StreamSignal<'_>),
        observe: &mut dyn FnMut(&agent_provider_config::protocol::StreamEvent),
    ) -> Result<tools::Answer, AskError> {
        use agent_provider_config::protocol::{Conversation as ModelConversation, ToolResult};
        let began = ui::time::now();
        let trace_id = uuid::Uuid::new_v4().as_u64_pair().0.max(1);
        self.trace.store(trace_id, Ordering::Relaxed);
        let mut revealed = Revealed::new();
        let first = action_user_turn(rest, catalogue, &mut revealed);
        let mut conversation = ModelConversation::new(ACTION_SYSTEM, first.text());
        for turn in 0..MAX_ACTION_TURNS {
            let timeout = self
                .timeout
                .checked_sub(began.elapsed())
                .ok_or_else(|| AskError::Failed("the call timed out".into()))?
                .min(MAX_TURN_TIME);
            live(StreamSignal::Turn(turn));
            let mut request = conversation.request(tools::typed_definitions(most_steps), 4096);
            first.apply_to(&mut request);
            let mut previews = NeutralPreviews::default();
            let mut scanners = BTreeMap::new();
            let mut reply = String::new();
            let response = tilo_model_gateway::complete_streaming(
                &provider.selection.profile_id,
                &provider.selection.model.id,
                request,
                timeout,
                &mut |event| {
                    observe(event);
                    if let Some(output) = previews.frame(event) {
                        stream_plan_delta(&mut scanners, &output, &mut |signal| {
                            // The plan's explicit user-facing reply is model output,
                            // unlike its decision explanation or action arguments.
                            if let StreamSignal::Reply { text, .. } = signal
                                && let Some(delta) = text.strip_prefix(&reply)
                                && !delta.is_empty()
                            {
                                observe(&agent_provider_config::protocol::StreamEvent::TextDelta {
                                    text: delta.into(),
                                });
                                text.clone_into(&mut reply);
                            }
                            live(signal);
                        });
                    }
                },
            )
            .map_err(|error| {
                let error = error.model_error();
                AskError::Failed(format!("{error} {}", error.recovery().guidance()))
            })?;
            let calls = conversation
                .validate_response(&response)
                .map_err(|error| AskError::Failed(error.to_string()))?;
            let calls: Vec<ToolCall> = calls.into_iter().cloned().collect();
            tools::validate_plan_calls(&calls).map_err(AskError::Failed)?;
            if calls.is_empty() {
                return Err(AskError::Failed("the model called no tool".into()));
            }
            if let Some(call) = calls.iter().find(|call| call.name == tools::PLAN) {
                match tools::read_plan(&call.input, &revealed, most_steps) {
                    Ok(answer) => return Ok(answer),
                    Err(error) => {
                        conversation
                            .push(
                                response,
                                vec![ToolResult {
                                    call_id: call.id.clone(),
                                    content: tools::plan_refused(&error),
                                    is_error: true,
                                }],
                            )
                            .map_err(|error| AskError::Failed(error.to_string()))?;
                        continue;
                    }
                }
            }
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                live(StreamSignal::ToolCall(&call.name));
                let answer = tools::answer(&call.name, &call.input, catalogue, &mut revealed);
                results.push(ToolResult {
                    call_id: call.id,
                    content: answer,
                    is_error: false,
                });
            }
            conversation
                .push(response, results)
                .map_err(|error| AskError::Failed(error.to_string()))?;
        }
        Err(AskError::Failed(
            "the model exhausted the action-discovery turn limit".into(),
        ))
    }
}

/// Sends one turn's request body and answers the provider's response body:
/// the gateway in production, a script in tests.
#[cfg(test)]
type PostTurn<'a> = dyn FnMut(
        Value,
        tilo_model_gateway::PlanTrace,
        &mut dyn FnMut(StreamSignal<'_>),
    ) -> Result<String, AskError>
    + 'a;

/// One action decision's conversation, whatever carries its turns.
#[cfg(test)]
struct ActionDecision<'a> {
    selection: &'a ProviderSelection,
    rest: &'a str,
    catalogue: &'a Catalogue,
    most_steps: usize,
}

#[cfg(test)]
impl ActionDecision<'_> {
    /// Run the conversation to a plan. The first turn already shows the
    /// published operations (and reveals them), and the system prompt and
    /// tools are the same bytes on every turn, so the provider's prompt cache
    /// keeps everything up to the newest messages — and, through the
    /// [`FirstTurn`]'s breakpoint, the operations across decisions too.
    fn run(
        &self,
        trace_id: u64,
        post: &mut PostTurn<'_>,
        live: &mut dyn FnMut(StreamSignal<'_>),
    ) -> Result<tools::Answer, AskError> {
        let dialect = Dialect::of(self.selection.protocol);
        let began = ui::time::now();
        let mut revealed = Revealed::new();
        let first = action_user_turn(self.rest, self.catalogue, &mut revealed);
        let mut messages = dialect.first_messages(&first);
        let mut refused = None;
        for turn in 0..MAX_ACTION_TURNS {
            let body = dialect.body(self.selection, &messages, self.most_steps);
            let turn_began = ui::time::now();
            let trace = tilo_model_gateway::PlanTrace {
                id: trace_id,
                turn: u32::try_from(turn).unwrap_or(u32::MAX),
            };
            live(StreamSignal::Turn(turn));
            let raw = post(body, trace, live);
            tracing::info!(
                trace = format!("{trace_id:016x}"),
                turn,
                protocol = ?self.selection.protocol,
                turn_ms = turn_began.elapsed().as_millis() as u64,
                total_ms = began.elapsed().as_millis() as u64,
                ok = raw.is_ok(),
                "model turn"
            );
            match dialect.parse(&raw?).map_err(AskError::Failed)? {
                ActionTurn::Plan {
                    assistant,
                    id,
                    input,
                } => match tools::read_plan(&input, &revealed, self.most_steps) {
                    Ok(answer) => return Ok(answer),
                    // Refused before anything ran. The model is told why —
                    // with the shown ids it may have meant — and may plan
                    // again, as a refused `invoke` is answered in the loop.
                    Err(error) => {
                        messages.push(assistant);
                        messages.push(dialect.tool_result(&id, &tools::plan_refused(&error)));
                        if dialect == Dialect::Anthropic {
                            fold_anthropic_results(&mut messages);
                        }
                        refused = Some(error);
                    }
                },
                ActionTurn::Tools { assistant, calls } => {
                    // The stream reported each call as it completed.
                    messages.push(assistant);
                    for call in calls {
                        let answer =
                            tools::answer(&call.name, &call.input, self.catalogue, &mut revealed);
                        messages.push(dialect.tool_result(&call.id, &answer));
                    }
                    if dialect == Dialect::Anthropic {
                        fold_anthropic_results(&mut messages);
                    }
                }
            }
        }
        Err(AskError::Failed(refused.map_or_else(
            || "the model exhausted the action-discovery turn limit".into(),
            |error| format!("the model exhausted the action-discovery turn limit: {error}"),
        )))
    }
}

pub use agent_provider_config::protocol::ToolCall;

#[cfg(test)]
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

/// A conversation's first user turn, in two parts: the published operations,
/// the same bytes for the same catalogue whatever the task, then everything
/// that varies per task (the earlier conversation, the document, the
/// request).
///
/// Anthropic caches only up to an explicit breakpoint, so its dialect sends
/// the parts as two text blocks with a breakpoint closing the operations:
/// the next task over the same catalogue reads tools, system prompt and
/// operations back from the cache instead of prefilling them again (the
/// gateway's own breakpoints still mark the conversation tail). An
/// `OpenAI`-compatible provider caches a repeated prefix by itself, and some
/// reject fields they do not know, so it gets the one joined string.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FirstTurn {
    /// The published operations: stable across tasks.
    pub operations: String,
    /// Everything after them.
    pub task: String,
}

impl FirstTurn {
    /// The whole turn as one text.
    pub fn text(&self) -> String {
        format!("{}{}", self.operations, self.task)
    }

    /// The first user message in `dialect`. A blank part gets no block (an
    /// empty text block is refused), and with no operations there is
    /// nothing worth a breakpoint of its own.
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

/// A first turn with no published operations ahead of `task`.
impl From<&str> for FirstTurn {
    fn from(task: &str) -> Self {
        Self {
            operations: String::new(),
            task: task.to_owned(),
        }
    }
}

/// Anthropic carries every `tool_result` of a turn in one user message:
/// gather the trailing result blocks pushed after the assistant turn.
#[cfg(test)]
fn fold_anthropic_results(messages: &mut Vec<Value>) {
    let start = messages
        .iter()
        .rposition(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .map_or(0, |index| index + 1);
    let results = messages.split_off(start);
    messages.push(serde_json::json!({ "role": "user", "content": results }));
}

#[cfg(test)]
fn action_request_body_for(
    selection: &ProviderSelection,
    messages: &[Value],
    most_steps: usize,
) -> Value {
    serde_json::json!({
        "model": selection.model.id,
        "max_tokens": 4096,
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
fn openai_action_request_body(
    selection: &ProviderSelection,
    messages: &[Value],
    most_steps: usize,
) -> Value {
    serde_json::json!({
        "model": selection.model.id,
        "max_completion_tokens": 4096,
        "messages": messages,
        "tools": openai_tools(&tools::definitions(most_steps)),
        "tool_choice": "required",
    })
}

/// `{name, description, input_schema}` → `{type: "function", function:
/// {name, description, parameters}}`.
#[cfg(test)]
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
fn parse_action_turn(body: &str) -> Result<ActionTurn, String> {
    let (assistant, calls) = anthropic_calls(body)?;
    settle_turn(assistant, calls)
}

/// The assistant turn (to replay) and its tool calls, in call order.
#[cfg(test)]
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
fn parse_openai_action_turn(body: &str) -> Result<ActionTurn, String> {
    let (assistant, calls) = openai_calls(body)?;
    settle_turn(assistant, calls)
}

/// The assistant turn (to replay) and its tool calls, in call order.
#[cfg(test)]
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
    if let Some(reasoning) = tilo_model_gateway::openai::reasoning_content(message)?
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

// ---------------------------------------------------------------------------
// Streamed loop turns
// ---------------------------------------------------------------------------

/// One event of a streamed loop turn, in arrival order.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnEvent {
    Neutral(agent_provider_config::protocol::StreamEvent),
    /// An intermediate gateway frame: `event` is
    /// [`tilo_model_gateway::EVENT_TOOL_CALL_DELTA`] or
    /// [`tilo_model_gateway::EVENT_TOOL_CALL`], `output` its JSON.
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
    /// The turn that owns (or last owned) the channel.
    turn: u64,
    /// That turn's Plan is over: nothing more of it is on the channel.
    over: bool,
    /// Held marks with the Unix ms at which each happened.
    pending: Vec<(
        tilo_model_gateway::PlanTrace,
        tilo_model_gateway::TraceEvent,
        u64,
    )>,
}

impl MarkState {
    /// Send the held marks, each with the time it happened, in order.
    fn flush(&mut self) {
        for (trace, event, at) in std::mem::take(&mut self.pending) {
            let _ = tilo_model_gateway::trace_at(trace, event, Some(at));
        }
    }
}

impl MarkGate {
    fn mark(&self, trace: tilo_model_gateway::PlanTrace, event: tilo_model_gateway::TraceEvent) {
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
        let _ = tilo_model_gateway::trace(trace, event);
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

/// Turns over the launcher-issued model gateway. Both dialects stream.
struct GatewayTurns {
    profile_id: String,
    model_id: String,
    marks: Arc<MarkGate>,
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
                        tilo_model_gateway::complete_streaming(
                            &profile_id,
                            &model_id,
                            neutral,
                            request.timeout,
                            &mut |event| {
                                let _ = tx.send(TurnEvent::Neutral(event.clone()));
                                if let Some(output) = previews.frame(event) {
                                    let _ = tx.send(TurnEvent::Frame {
                                        event: tilo_model_gateway::EVENT_TOOL_CALL_DELTA.into(),
                                        output,
                                    });
                                }
                            },
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

/// Provider-neutral history for the discover → invoke → observe loop.
#[derive(Clone, Debug)]
pub struct Conversation {
    history: agent_provider_config::protocol::Conversation,
    directions: Vec<(usize, String)>,
    first: Option<FirstTurn>,
    #[cfg(test)]
    legacy: Option<LegacyConversation>,
}

impl Conversation {
    /// Compatibility constructor; provider selection belongs to the gateway.
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
            #[cfg(test)]
            legacy: None,
        }
    }

    pub fn streams(&self) -> bool {
        #[cfg(test)]
        if let Some(legacy) = &self.legacy {
            return legacy.streams();
        }
        true
    }

    pub fn body(&self, tools: &[agent_provider_config::protocol::ToolDefinition]) -> Value {
        #[cfg(test)]
        if let Some(legacy) = &self.legacy {
            return legacy.body(tools);
        }
        let mut request = self.history.request(tools.to_vec(), 4096);
        if let Some(first) = &self.first {
            first.apply_to(&mut request);
        }
        for (offset, (index, text)) in self.directions.iter().enumerate() {
            request.messages.insert(
                index + offset,
                agent_provider_config::protocol::Message::text(
                    agent_provider_config::protocol::Role::User,
                    text,
                ),
            );
        }
        serde_json::json!(request)
    }

    /// Validate the complete response before any tools may execute.
    ///
    /// # Errors
    /// Rejects malformed, truncated, empty or replayed tool batches.
    pub fn calls(&self, body: &str) -> Result<(Value, Vec<ToolCall>), String> {
        #[cfg(test)]
        if let Some(legacy) = &self.legacy {
            return legacy.calls(body);
        }
        let response: agent_provider_config::protocol::ModelResponse =
            serde_json::from_str(body).map_err(|_| "invalid gateway completion".to_string())?;
        let calls = self
            .history
            .validate_response(&response)
            .map_err(|error| error.to_string())?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        if calls.is_empty() {
            return Err("the model called no tool".into());
        }
        tools::validate_loop_calls(&calls)?;
        Ok((serde_json::json!(response), calls))
    }

    /// Append a validated response and its ordered results atomically.
    ///
    /// # Errors
    /// Rejects malformed responses, mismatched results and consumed call IDs.
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
        self.directions
            .push((self.history.messages().len(), text.into()));
    }

    pub fn len(&self) -> usize {
        #[cfg(test)]
        if let Some(legacy) = &self.legacy {
            return legacy.messages.len();
        }
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
    /// definitions, re-enveloped for the `OpenAI` dialect).
    pub fn body(&self, tools: &[agent_provider_config::protocol::ToolDefinition]) -> Value {
        let tools = serde_json::json!(tools);
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
                "tools": openai_tools(&tools),
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
        tools::validate_loop_calls(&calls)?;
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

    pub fn push_user(&mut self, text: &str) {
        self.messages
            .push(serde_json::json!({"role":"user", "content":text}));
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

/// Adapt neutral deltas to the existing presentation scanner. Providers may
/// supply the name only on the first delta; retain it for subsequent chunks.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Instruction;
    use crate::surface::Offer;
    use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
    use tilo_actions::{ActionContract, ActionParam, ExecutionLocus, OperationKind, ParamType};

    #[test]
    fn managed_conversations_use_neutral_requests_and_ordered_results() {
        use agent_provider_config::protocol::{Content, Message, ModelResponse, Role, StopReason};
        let mut conversation = Conversation::neutral("system", "user");
        let tools = tools::typed_loop_definitions();
        let request = conversation.body(&tools);
        assert!(request.get("model").is_none());
        assert!(request.get("max_tokens").is_none());
        assert_eq!(request["max_output_tokens"], 4096);
        assert_eq!(request["tools"][3]["name"], tools::INVOKE);
        let response = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "call-a".into(),
                    name: tools::LIST_APPLICATIONS.into(),
                    input: serde_json::json!({}),
                    extra_content: None,
                })],
            },
            stop_reason: StopReason::ToolCalls,
            usage: agent_provider_config::protocol::Usage::default(),
        };
        let (assistant, calls) = conversation
            .calls(&serde_json::json!(response).to_string())
            .unwrap();
        assert_eq!(calls[0].id, "call-a");
        conversation
            .push(
                assistant,
                vec![("call-a".into(), "visible applications".into())],
            )
            .unwrap();
        let next = conversation.body(&tools);
        assert_eq!(next["messages"][3]["content"][0]["call_id"], "call-a");
        assert!(
            conversation
                .calls(&serde_json::json!(response).to_string())
                .is_err(),
            "consumed call ids cannot be replayed"
        );
    }

    #[test]
    fn neutral_history_preserves_catalogue_blocks_and_authenticated_steering() {
        let first = FirstTurn {
            operations: "stable catalogue".into(),
            task: "private instruction".into(),
        };
        let mut conversation = Conversation::neutral_first("system", &first);
        conversation.push_user("authenticated direction");
        let request = conversation.body(&tools::typed_loop_definitions());
        assert_eq!(
            request["messages"][1]["content"][0]["text"],
            "stable catalogue"
        );
        assert_eq!(
            request["messages"][1]["content"][1]["text"],
            "private instruction"
        );
        assert_eq!(request["messages"][2]["role"], "user");
        assert_eq!(
            request["messages"][2]["content"][0]["text"],
            "authenticated direction"
        );
        assert_eq!(
            request["tools"][3]["input_schema"]["properties"][tools::INVOKE_STANDALONE]["type"],
            "boolean"
        );
    }

    #[test]
    fn neutral_stream_previews_retain_names_and_never_supply_calls() {
        use agent_provider_config::protocol::StreamEvent;
        let mut previews = NeutralPreviews::default();
        let first = previews
            .frame(&StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("a".into()),
                name: Some(tools::INVOKE.into()),
                arguments: "{\"operation\":\"write\",".into(),
            })
            .unwrap();
        let next = previews
            .frame(&StreamEvent::ToolCallDelta {
                index: 0,
                id: None,
                name: None,
                arguments: "\"parameters\":{\"text\":\"preview\"}}".into(),
            })
            .unwrap();
        let mut scanners = BTreeMap::new();
        let mut saw_preview = false;
        for frame in [first, next] {
            stream_plan_delta(&mut scanners, &frame, &mut |signal| {
                if matches!(
                    signal,
                    StreamSignal::CallArgument {
                        text: "preview",
                        ..
                    }
                ) {
                    saw_preview = true;
                }
            });
        }
        assert!(saw_preview);
        assert!(
            previews
                .frame(&StreamEvent::Failed {
                    error: agent_provider_config::protocol::ModelError::invalid("reset"),
                })
                .is_none()
        );
        assert!(Conversation::neutral("system", "user").streams());
    }

    #[test]
    fn live_tool_transcript_selects_an_arbitrary_typed_app_action() {
        let offer = Offer {
            app: "calculator".to_string(),
            broker_peer_id: None,
            broker_provider: None,
            descriptor: ActionDescriptor {
                semantic_identity: None,
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
        let encoded = serde_json::to_string(&tools::definitions(4)).unwrap();
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
        let ActionTurn::Plan { input, .. } = parse_action_turn(&final_turn).unwrap() else {
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
    fn reasoning_history_stays_in_the_original_coding_conversation() {
        let protocol = agent_provider_config::ProviderProtocol::OpenAiCompatible;
        let mut conversation =
            Conversation::new(protocol, "pinned-qwen", "system", &FirstTurn::from("task"));
        let body = serde_json::json!({"choices":[{"message":{
            "content":null,"reasoning_content":"provider-authored thought",
            "tool_calls":[{"id":"call_1","type":"function","function":{"name":"list_operations","arguments":"{}"}}]
        }}]}).to_string();
        let (assistant, calls) = conversation.calls(&body).unwrap();
        assert_eq!(calls.len(), 1);
        conversation
            .push(assistant, vec![("call_1".into(), "observed result".into())])
            .unwrap();
        let next = conversation.body(&[]);
        assert_eq!(
            next["messages"][2]["reasoning_content"],
            "provider-authored thought"
        );
        assert_eq!(next["messages"][2]["content"], Value::Null);
        assert_eq!(next["messages"][3]["tool_call_id"], "call_1");
        let fresh = Conversation::new(
            protocol,
            "different-model",
            "system",
            &FirstTurn::from("new task"),
        );
        assert!(
            !fresh
                .body(&[])
                .to_string()
                .contains("provider-authored thought")
        );
        let other_dialect = Conversation::new(
            agent_provider_config::ProviderProtocol::Anthropic,
            "other-provider",
            "system",
            &FirstTurn::from("new task"),
        );
        assert!(
            !other_dialect
                .body(&[])
                .to_string()
                .contains("reasoning_content")
        );
        let mut malformed: Value = serde_json::from_str(&body).unwrap();
        malformed["choices"][0]["message"]["reasoning_content"] =
            serde_json::json!({"wrong":"type"});
        assert!(conversation.calls(&malformed.to_string()).is_err());
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
        let mut messages =
            dialect.first_messages(&FirstTurn::from("turn the bullets into a table"));
        let body = dialect.body(&selection, &messages, 4);

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
        let definitions = tools::definitions(4);
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
        let ActionTurn::Plan { input, .. } = dialect.parse(&plan_turn).unwrap() else {
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
            &Catalogue::default(),
            &mut Revealed::new(),
        ));
        let body = Dialect::OpenAi.body(&selection, &messages, 8);
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
        let mut messages = dialect.first_messages(&FirstTurn::from("do it"));
        let body = dialect.body(&selection, &messages, 4);
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

    /// `Finished` goes out before the marks a turn held. The next turn may
    /// start in between: it sends them before taking the channel, and the
    /// finished turn's late release then leaves the channel to it.
    #[test]
    fn a_late_release_never_frees_the_channel_a_later_turn_holds() {
        let gate = MarkGate::default();
        let trace = tilo_model_gateway::PlanTrace { id: 1, turn: 0 };
        let held = |gate: &MarkGate| {
            let state = gate.state.lock().unwrap();
            (state.busy, state.pending.len())
        };
        let first = gate.acquire();
        gate.mark(trace, tilo_model_gateway::TraceEvent::ActionSubmitted);
        assert_eq!(
            held(&gate),
            (true, 1),
            "held while the turn owns the channel"
        );
        // The loop already heard `Finished` and starts the next turn before
        // the first one's thread released.
        gate.plan_over(first);
        let second = gate.acquire();
        assert_eq!(held(&gate), (true, 0), "sent before the next Plan starts");
        gate.release(first);
        assert_eq!(
            held(&gate),
            (true, 0),
            "the next turn still owns the channel"
        );
        gate.mark(trace, tilo_model_gateway::TraceEvent::ActionSettled);
        assert_eq!(held(&gate), (true, 1));
        // A turn whose Plan is not over (abandoned, still streaming) keeps
        // its marks from the channel when another turn starts.
        let third = gate.acquire();
        assert_eq!(held(&gate), (true, 1));
        gate.release(second);
        assert_eq!(held(&gate), (true, 1));
        gate.plan_over(third);
        gate.release(third);
        assert_eq!(held(&gate), (false, 0), "released: sent, channel free");
    }

    const PROTOCOLS: [agent_provider_config::ProviderProtocol; 2] = [
        agent_provider_config::ProviderProtocol::Anthropic,
        agent_provider_config::ProviderProtocol::OpenAiCompatible,
    ];
    const SAMPLE: &str = "calculator.range:main#sample";

    /// A room whose calculator publishes one typed operation.
    fn calculator() -> Catalogue {
        Catalogue {
            offers: vec![Offer {
                app: "calculator".to_string(),
                broker_peer_id: None,
                broker_provider: None,
                descriptor: ActionDescriptor {
                    semantic_identity: None,
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
            }],
            facts: Vec::new(),
            held: vec!["calculator"],
        }
    }

    /// One provider response calling `name` with `input`, in `protocol`'s
    /// dialect.
    fn call(
        protocol: agent_provider_config::ProviderProtocol,
        id: &str,
        name: &str,
        input: &Value,
    ) -> String {
        match Dialect::of(protocol) {
            Dialect::Anthropic => serde_json::json!({
                "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": id, "name": name, "input": input}]
            }),
            Dialect::OpenAi => serde_json::json!({
                "choices": [{
                    "finish_reason": "tool_calls",
                    "message": {"role": "assistant", "content": null, "tool_calls": [
                        {"id": id, "type": "function", "function": {"name": name, "arguments": input.to_string()}}
                    ]}
                }]
            }),
        }
        .to_string()
    }

    fn plan_of(operation: &str) -> Value {
        serde_json::json!({
            "steps": [{"operation": operation, "arguments": {"count": 12, "normalise": true}}],
            "why": "the calculator publishes the sampler",
            "then": null
        })
    }

    /// Run one action decision over the production conversation, the
    /// provider answering `answers` in order. The decision and every request
    /// body sent.
    fn decide(
        protocol: agent_provider_config::ProviderProtocol,
        answers: Vec<String>,
    ) -> (Result<tools::Answer, String>, Vec<Value>) {
        decide_on(protocol, "sample twelve values, normalised", answers)
    }

    /// [`decide`] for the instruction `rest`, over the same catalogue.
    fn decide_on(
        protocol: agent_provider_config::ProviderProtocol,
        rest: &str,
        answers: Vec<String>,
    ) -> (Result<tools::Answer, String>, Vec<Value>) {
        let mut profile = agent_provider_config::ProviderProfile::hosted(protocol);
        profile
            .models
            .push(agent_provider_config::ModelDescriptor::new("scripted"));
        let selection = ProviderSelection::from_profile(&profile, profile.models[0].clone());
        let catalogue = calculator();
        let decision = ActionDecision {
            selection: &selection,
            rest,
            catalogue: &catalogue,
            most_steps: 4,
        };
        let mut answers = answers.into_iter();
        let mut sent = Vec::new();
        let decided = decision.run(
            7,
            &mut |body, _, _| {
                sent.push(body);
                answers
                    .next()
                    .ok_or_else(|| AskError::Failed("the script ran out of turns".into()))
            },
            &mut |_| {},
        );
        (decided.map_err(|AskError::Failed(error)| error), sent)
    }

    /// The text of the first user turn of a request: one string, or
    /// Anthropic's text blocks in order.
    fn first_user_turn(body: &Value) -> String {
        let user = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["role"] == "user")
            .unwrap();
        match &user["content"] {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .map(|block| {
                    assert_eq!(block["type"], "text", "{block}");
                    block["text"].as_str().unwrap()
                })
                .collect(),
            other => panic!("a first user turn is text: {other}"),
        }
    }

    /// Two decisions over the same catalogue open with byte-identical
    /// operations. Anthropic's first turn closes them with a cache
    /// breakpoint of their own, so the next decision reads tools, system
    /// prompt and operations back from the cache; the document and the
    /// instruction follow in a block of their own. `OpenAI`-compatible
    /// bodies carry no `cache_control` at all and keep the one string.
    #[test]
    fn the_operations_are_cached_across_decisions() {
        let first_bodies = |protocol: agent_provider_config::ProviderProtocol| {
            ["sample twelve values, normalised", "sample three values"].map(|rest| {
                let answer = call(protocol, "p1", tools::PLAN, &plan_of(SAMPLE));
                let (decided, sent) = decide_on(protocol, rest, vec![answer]);
                assert_eq!(decided.unwrap().actions, [SAMPLE]);
                sent.into_iter().next().unwrap()
            })
        };
        let anthropic = first_bodies(agent_provider_config::ProviderProtocol::Anthropic);
        // What Anthropic caches at the breakpoint, in its render order:
        // tools, system, then the first user message through the operations.
        let prefix = |body: &Value| {
            let content = body["messages"][0]["content"].as_array().unwrap();
            assert_eq!(content.len(), 2, "{body}");
            serde_json::to_string(&(&body["tools"], &body["system"], &content[0])).unwrap()
        };
        assert_eq!(prefix(&anthropic[0]), prefix(&anthropic[1]));
        let operations = &anthropic[0]["messages"][0]["content"][0];
        assert_eq!(operations["type"], "text");
        assert_eq!(
            operations["cache_control"],
            serde_json::json!({"type": "ephemeral"})
        );
        let operations = operations["text"].as_str().unwrap();
        assert!(
            operations.starts_with("Operations published right now:")
                && operations.contains(SAMPLE),
            "{operations}"
        );
        assert!(
            !operations.contains("The instruction asks for"),
            "{operations}"
        );
        let task = |body: &Value| body["messages"][0]["content"][1].clone();
        assert_ne!(task(&anthropic[0]), task(&anthropic[1]));
        assert!(task(&anthropic[0]).get("cache_control").is_none());
        assert!(
            task(&anthropic[1])["text"]
                .as_str()
                .unwrap()
                .ends_with("The instruction asks for:\n\nsample three values\n")
        );
        for body in &anthropic {
            // The one breakpoint the agent places; the gateway adds the rest.
            assert_eq!(body.to_string().matches("cache_control").count(), 1);
        }

        let openai = first_bodies(agent_provider_config::ProviderProtocol::OpenAiCompatible);
        for (body, same) in openai.iter().zip(&anthropic) {
            assert!(!body.to_string().contains("cache_control"), "{body}");
            assert!(body["messages"][1]["content"].is_string(), "{body}");
            // The same words, joined.
            assert_eq!(first_user_turn(body), first_user_turn(same));
            assert!(first_user_turn(body).starts_with(operations));
        }
    }

    /// The first turn already shows the published operations with their
    /// parameters (and reveals them), so a request the summary answers is
    /// planned in ONE provider call — no `list_*`/`describe_operation`
    /// round trips.
    #[test]
    fn a_request_the_summary_answers_is_planned_in_one_model_turn() {
        for protocol in PROTOCOLS {
            let (decided, sent) = decide(
                protocol,
                vec![call(protocol, "p1", tools::PLAN, &plan_of(SAMPLE))],
            );
            let answer = decided.unwrap_or_else(|error| panic!("{protocol:?}: {error}"));
            assert_eq!(sent.len(), 1, "{protocol:?}: one provider call");
            assert_eq!(answer.actions, [SAMPLE]);
            assert_eq!(answer.arguments[0]["count"], 12);
            assert_eq!(answer.arguments[0]["normalise"], true);
            let user = first_user_turn(&sent[0]);
            assert!(
                user.contains(&format!(
                    "{SAMPLE} — Sample values {{count: integer, normalise: boolean}}"
                )),
                "{protocol:?}: {user}"
            );
            // Stable first, volatile last: the operations, then the
            // instruction.
            let operations = user.find(SAMPLE).unwrap();
            let instruction = user.find("The instruction asks for").unwrap();
            assert!(operations < instruction, "{user}");
        }
    }

    /// With no enum to enforce it, the check behind `plan` is `read_plan`:
    /// an id nothing showed is refused back to the model with the ids it may
    /// have meant, the model may plan again, and a model that never names a
    /// shown id still ends within the turn limit with the reason.
    #[test]
    fn a_plan_naming_an_unshown_operation_is_refused_back_to_the_model() {
        for protocol in PROTOCOLS {
            let invented = "calculator.range:other#sample";
            let (decided, sent) = decide(
                protocol,
                vec![
                    call(protocol, "p1", tools::PLAN, &plan_of(invented)),
                    call(protocol, "p2", tools::PLAN, &plan_of(SAMPLE)),
                ],
            );
            let answer = decided.unwrap_or_else(|error| panic!("{protocol:?}: {error}"));
            assert_eq!(answer.actions, [SAMPLE]);
            assert_eq!(sent.len(), 2);
            let messages = sent[1]["messages"].as_array().unwrap();
            let refusal = messages.last().unwrap();
            let (call_id, text) = match Dialect::of(protocol) {
                Dialect::Anthropic => {
                    assert_eq!(refusal["role"], "user");
                    (
                        refusal["content"][0]["tool_use_id"].as_str(),
                        refusal["content"][0]["content"].as_str().unwrap(),
                    )
                }
                Dialect::OpenAi => {
                    assert_eq!(refusal["role"], "tool");
                    (
                        refusal["tool_call_id"].as_str(),
                        refusal["content"].as_str().unwrap(),
                    )
                }
            };
            assert_eq!(
                call_id,
                Some("p1"),
                "{protocol:?}: answers the refused call"
            );
            assert!(text.contains("nothing was performed"), "{text}");
            assert!(text.contains(invented), "{text}");
            assert!(
                text.contains(&format!("Listed ids for that action: {SAMPLE}")),
                "{protocol:?}: suggests the shown id: {text}"
            );
            assert!(text.contains(tools::LIST_OPERATIONS), "{text}");

            let stubborn =
                vec![call(protocol, "p", tools::PLAN, &plan_of(invented)); MAX_ACTION_TURNS];
            let (decided, sent) = decide(protocol, stubborn);
            let error = decided.expect_err("never shown, never planned");
            assert_eq!(sent.len(), MAX_ACTION_TURNS);
            assert!(
                error.contains("no listing in this conversation returned"),
                "{protocol:?}: {error}"
            );
        }
    }

    /// Everything before the newest messages is the same bytes on every turn
    /// of one decision — system prompt, tools, and each earlier message — so
    /// the provider's prompt cache (`OpenAI`'s automatic one too) hits the
    /// whole prefix. Revealing operations no longer rewrites `plan`.
    #[test]
    fn the_system_prompt_and_tools_are_the_same_bytes_on_every_turn() {
        for protocol in PROTOCOLS {
            let (decided, sent) = decide(
                protocol,
                vec![
                    call(
                        protocol,
                        "l1",
                        tools::LIST_OPERATIONS,
                        &serde_json::json!({"application": "calculator"}),
                    ),
                    call(
                        protocol,
                        "d1",
                        tools::DESCRIBE_OPERATION,
                        &serde_json::json!({"operation": SAMPLE}),
                    ),
                    call(protocol, "p1", tools::PLAN, &plan_of(SAMPLE)),
                ],
            );
            assert_eq!(decided.unwrap().actions, [SAMPLE]);
            assert_eq!(sent.len(), 3);
            let tool_bytes = |body: &Value| serde_json::to_string(&body["tools"]).unwrap();
            let system = |body: &Value| match Dialect::of(protocol) {
                Dialect::Anthropic => serde_json::to_string(&body["system"]).unwrap(),
                Dialect::OpenAi => serde_json::to_string(&body["messages"][0]).unwrap(),
            };
            for later in &sent[1..] {
                assert_eq!(
                    tool_bytes(later),
                    tool_bytes(&sent[0]),
                    "{protocol:?}: tools"
                );
                assert_eq!(system(later), system(&sent[0]), "{protocol:?}: system");
            }
            assert!(
                !tool_bytes(&sent[0]).contains("\"enum\""),
                "{}",
                tool_bytes(&sent[0])
            );
            for pair in sent.windows(2) {
                let earlier = pair[0]["messages"].as_array().unwrap();
                let later = pair[1]["messages"].as_array().unwrap();
                assert!(later.len() > earlier.len());
                assert_eq!(
                    serde_json::to_string(&later[..earlier.len()]).unwrap(),
                    serde_json::to_string(earlier).unwrap(),
                    "{protocol:?}: earlier messages are replayed unchanged"
                );
            }
        }
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
            assert!(!scanner.failed, "{pieces:?}");
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
            assert!(scanner.failed);
            assert_eq!(reports, 0);
            let mut nested = PlanArgumentsScanner::new();
            nested.feed(r#"{"reply":{"text":"x"}}"#, &mut |_, _, _| reports += 1);
            assert!(!nested.failed);
            assert_eq!(reports, 0, "only the top-level string reply is followed");
        }
    }
}
