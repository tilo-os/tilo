//! What carries a model's turns, holding its credential, and the usage log
//! every turn is written to.
//!
//! Whatever carries the turns ([`PlanTurns`]: the gateway for a Settings
//! provider, the trusted host's own client for an endpoint it was given)
//! holds the credential; its callers see requests and responses only.
//! Every turn is logged to `TILO_AGENT_USAGE_LOG` when set, with the
//! provider's token counts, why it stopped (a refusal included) and how many
//! pictures it carried, never their bytes.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer, Serialize};
use tilo_model_gateway::host_backend::protocol::{
    Content, ErrorKind, ModelError, ModelRequest, ModelResponse, StopReason, StreamEvent,
};

/// The most characters of a refusal's own words kept in a usage line.
const MOST_REFUSAL_CHARS: usize = 300;
/// Milliseconds in one UTC day, for daily usage rollups.
const MS_PER_DAY: u64 = 86_400_000;

/// What woke a model call, for per-cause token and spend aggregation.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CauseKind {
    /// A person-started conversation turn (also the default for older log lines without a cause).
    #[default]
    Turn,
    /// An event-triggered wake.
    Event,
    /// A look-round check.
    Look,
    /// A bounded reflection pass.
    Reflection,
    /// A shadow evaluation call.
    Shadow,
}

/// Structured cause attached to every model usage line.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallCause {
    /// Broad category of what woke the model call.
    #[serde(default)]
    pub kind: CauseKind,
    /// Optional identifier for the event, task, or check that woke the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Optional human-readable name for the event or check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional task identifier for rolling spend across sub-agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl CallCause {
    /// A person-started conversation turn.
    #[must_use]
    pub fn turn() -> Self {
        Self {
            kind: CauseKind::Turn,
            ..Self::default()
        }
    }

    /// An event-triggered wake identified by `id`.
    #[must_use]
    pub fn event(id: impl Into<String>) -> Self {
        Self {
            kind: CauseKind::Event,
            id: Some(id.into()),
            ..Self::default()
        }
    }

    /// A look-round check.
    #[must_use]
    pub fn look() -> Self {
        Self {
            kind: CauseKind::Look,
            ..Self::default()
        }
    }

    /// A bounded reflection pass.
    #[must_use]
    pub fn reflection() -> Self {
        Self {
            kind: CauseKind::Reflection,
            ..Self::default()
        }
    }

    /// A shadow evaluation call.
    #[must_use]
    pub fn shadow() -> Self {
        Self {
            kind: CauseKind::Shadow,
            ..Self::default()
        }
    }

    /// Attach a specific identifier to this cause.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Attach a human-readable name to this cause.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Attach a task identifier for multi-agent spend rollups.
    #[must_use]
    pub fn with_task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }
}

thread_local! {
    static CURRENT_CAUSE: RefCell<CallCause> = RefCell::new(CallCause::default());
}

/// The active cause on the current thread (defaults to [`CauseKind::Turn`]).
#[must_use]
pub fn current_call_cause() -> CallCause {
    CURRENT_CAUSE.with(|cell| cell.borrow().clone())
}

/// Execute `work` with `cause` set as the current thread's call cause, restoring the prior cause
/// when `work` finishes (even on unwind).
#[allow(
    dead_code,
    reason = "foundation API for tagging scoped model calls with their cause"
)]
pub fn with_call_cause<T>(cause: CallCause, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<CallCause>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(previous) = self.0.take() {
                CURRENT_CAUSE.with(|cell| {
                    *cell.borrow_mut() = previous;
                });
            }
        }
    }
    let previous = CURRENT_CAUSE.with(|cell| cell.replace(cause));
    let _guard = Restore(Some(previous));
    work()
}

/// Deserialize `cause`, treating both an absent field and explicit JSON `null` as the default
/// [`CallCause`] (`turn`).
fn deserialize_cause<'de, D>(deserializer: D) -> Result<CallCause, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<CallCause>::deserialize(deserializer).map(Option::unwrap_or_default)
}

/// What carries a model's turns, holding the credential.
pub trait PlanTurns: Send {
    /// Answer one turn's `request`. `key` names the turn, the same for
    /// every attempt at it, so a metered endpoint bills it once; `retry`
    /// says this is the turn asked again after its first call was lost to
    /// the network, for the usage log.
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        retry: bool,
    ) -> Result<ModelResponse, ModelError>;

    /// [`Self::turn`], handing what the model writes to `written` while it writes: the
    /// answer's words as they arrive, for whoever shows the answer growing. Presentation
    /// only: the answer returned is what counts. What carries its turns without anything
    /// to hand on answers whole, and `written` hears nothing.
    fn turn_streaming(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
        retry: bool,
        written: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<ModelResponse, ModelError> {
        let _ = written;
        self.turn(request, key, deadline, retry)
    }

    /// The model's name, for the usage log.
    fn model(&self) -> &str;

    /// Whether the model is shown the pictures a tool answers with. One that
    /// is not reads that it was not shown them.
    fn sees(&self) -> bool {
        false
    }

    /// What woke turns carried by this provider, for the usage log.
    fn cause(&self) -> CallCause {
        current_call_cause()
    }
}

/// The Settings provider's turns, through the per-user model gateway.
pub struct GatewayTurns {
    profile_id: String,
    model_id: String,
    sees: bool,
    cause: Option<CallCause>,
}

impl GatewayTurns {
    /// Turns over `selection`'s profile and model.
    #[must_use]
    pub fn new(selection: tilo_agent::providers::ProviderSelection) -> Self {
        Self {
            profile_id: selection.profile_id,
            sees: selection.model.vision,
            model_id: selection.model.id,
            cause: None,
        }
    }

    /// Tag every model call made through this provider with `cause`.
    #[must_use]
    pub fn with_cause(mut self, cause: CallCause) -> Self {
        self.cause = Some(cause);
        self
    }

    /// Update the cause tagged on subsequent model calls made through this provider.
    pub fn set_cause(&mut self, cause: CallCause) {
        self.cause = Some(cause);
    }

    fn effective_cause(&self) -> CallCause {
        self.cause.clone().unwrap_or_else(current_call_cause)
    }
}

impl PlanTurns for GatewayTurns {
    fn turn(
        &mut self,
        request: &ModelRequest,
        _: &str,
        deadline: Instant,
        retry: bool,
    ) -> Result<ModelResponse, ModelError> {
        if !tilo_model_gateway::available() {
            return Err(ModelError::new(ErrorKind::Unavailable));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ModelError::new(ErrorKind::TimedOut));
        }
        let started = Instant::now();
        let answer = tilo_model_gateway::complete_streaming(
            &self.profile_id,
            &self.model_id,
            request.clone(),
            left,
            &mut |_| {},
        )
        .map_err(|error| error.model_error());
        let cause = self.effective_cause();
        write_usage(
            (&self.model_id, None),
            request,
            started.elapsed(),
            &answer,
            Attempt {
                hedge: false,
                retry,
            },
            &cause,
        );
        answer
    }

    fn model(&self) -> &str {
        &self.model_id
    }

    fn sees(&self) -> bool {
        self.sees
    }

    fn cause(&self) -> CallCause {
        self.effective_cause()
    }
}

/// Which call of a turn a usage line counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Attempt {
    /// The same request sent again while the first was slow.
    pub hedge: bool,
    /// The turn asked again after its first call was lost to the network.
    pub retry: bool,
}

/// Appends one JSON line per model call to `TILO_AGENT_USAGE_LOG`, when
/// set: which agent asked (its thread's name), how long it took, the
/// provider's token counts, why it stopped and a refusal's words, how many
/// pictures it carried and their encoded bytes (never the pictures), and
/// whether it was a hedge or a lost call asked again. Benchmarks price runs
/// from these lines.
pub(crate) fn record_usage(
    (model, tier): (&str, Option<&str>),
    request: &ModelRequest,
    elapsed: Duration,
    answer: &Result<ModelResponse, ModelError>,
    attempt: Attempt,
) {
    let cause = current_call_cause();
    write_usage((model, tier), request, elapsed, answer, attempt, &cause);
}

/// Build and append one JSON usage entry for a model call with explicit `cause`.
fn write_usage(
    (model, tier): (&str, Option<&str>),
    request: &ModelRequest,
    elapsed: Duration,
    answer: &Result<ModelResponse, ModelError>,
    attempt: Attempt,
    cause: &CallCause,
) {
    let Some(path) = std::env::var_os("TILO_AGENT_USAGE_LOG") else {
        return;
    };
    let agent = std::thread::current().name().map(str::to_owned);
    let entry = usage_entry(
        (model, tier),
        agent.as_deref(),
        (elapsed, pictures_in(request)),
        answer,
        attempt,
        cause,
    );
    if let Err(error) = append_usage_entry(Path::new(&path), &entry) {
        tracing::warn!(%error, "cannot write model usage");
    }
}

/// Append one serialized [`UsageEntry`] JSON line to `path` in a single write.
fn append_usage_entry(path: &Path, entry: &UsageEntry) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut encoded = serde_json::to_string(entry)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    encoded.push('\n');
    // One write per line: `writeln!` writes in pieces, and agents in other
    // processes append to the same log at once.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(encoded.as_bytes()))
}

/// How many pictures `request` carries, and their bytes as sent (encoded):
/// for the usage log, which never holds the pictures.
fn pictures_in(request: &ModelRequest) -> (usize, usize) {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|content| match content {
            Content::Image { base64, .. } => Some(base64.len()),
            _ => None,
        })
        .fold((0_usize, 0_usize), |(count, bytes), size| {
            (count.saturating_add(1), bytes.saturating_add(size))
        })
}

/// The refusal a response stands for, in its own words (empty when it gave
/// none): a refusal often arrives with no content at all, which read as an
/// empty answer would hide why the model gave none.
fn refusal(response: &ModelResponse) -> Option<String> {
    (response.stop_reason == StopReason::Refusal).then(|| {
        response
            .message
            .content
            .iter()
            .filter_map(|block| match block {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>()
            .trim()
            .chars()
            .take(MOST_REFUSAL_CHARS)
            .collect()
    })
}

/// Input token cache breakdown in the Responses API shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputTokensDetails {
    /// Input tokens read from the provider's prompt cache.
    #[serde(default)]
    pub cached_tokens: u64,
    /// Input tokens written into the provider's prompt cache.
    #[serde(default)]
    pub cache_write_tokens: u64,
}

/// Output token reasoning breakdown in the Responses API shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputTokensDetails {
    /// Output tokens spent reasoning.
    #[serde(default)]
    pub reasoning_tokens: u64,
}

/// A call's token counts as the usage log keeps them, in the Responses
/// API's shape: every input token, of which `input_tokens_details` read
/// from and wrote to the provider's cache, and every output token, of which
/// `output_tokens_details` reasoned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageCounts {
    /// Every input token (uncached, cache-read, and cache-write).
    #[serde(default)]
    pub input_tokens: u64,
    /// Every output token (including reasoning).
    #[serde(default)]
    pub output_tokens: u64,
    /// Provider-reported total tokens.
    #[serde(default)]
    pub total_tokens: u64,
    /// Prompt-cache input token breakdown.
    #[serde(default)]
    pub input_tokens_details: InputTokensDetails,
    /// Reasoning output token breakdown.
    #[serde(default)]
    pub output_tokens_details: OutputTokensDetails,
}

impl UsageCounts {
    fn from_protocol(usage: &tilo_model_gateway::host_backend::protocol::Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
            input_tokens_details: InputTokensDetails {
                cached_tokens: usage.cached_input_tokens,
                cache_write_tokens: usage.cache_write_tokens,
            },
            output_tokens_details: OutputTokensDetails {
                reasoning_tokens: usage.reasoning_tokens,
            },
        }
    }
}

/// One JSON line appended to `TILO_AGENT_USAGE_LOG` per model call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageEntry {
    /// Wall-clock timestamp in milliseconds since the Unix epoch.
    pub at_unix_ms: u64,
    /// Agent thread name, when set.
    pub agent: Option<String>,
    /// Model identifier.
    pub model: String,
    /// Call duration in milliseconds.
    pub elapsed_ms: u64,
    /// Whether the provider call succeeded.
    pub ok: bool,
    /// Error message when `ok` is `false`.
    pub error: Option<String>,
    /// Token counts reported by the provider.
    pub usage: Option<UsageCounts>,
    /// Spend for this call in millionths of the currency.
    #[serde(default)]
    pub cost_micros: u64,
    /// What woke this call; defaults cleanly to [`CauseKind::Turn`] for older log lines.
    #[serde(default, deserialize_with = "deserialize_cause")]
    pub cause: CallCause,
    /// Why the model stopped (a refusal, a cut-off), so an empty answer
    /// is told from a refused one.
    pub stop_reason: Option<StopReason>,
    /// Leading words of a refusal, when `stop_reason` is `Refusal`.
    pub refusal: Option<String>,
    /// Number of images attached to the request.
    #[serde(default)]
    pub pictures: usize,
    /// Total base64 bytes of images attached to the request (never image bytes themselves).
    #[serde(default)]
    pub picture_bytes: usize,
    /// Whether this call was a hedged duplicate.
    #[serde(default)]
    pub hedge: bool,
    /// Whether this call retried a lost request.
    #[serde(default)]
    pub retry: bool,
    /// Optional provider service tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

impl UsageEntry {
    /// Whole UTC days since the Unix epoch (`at_unix_ms / 86_400_000`).
    #[must_use]
    #[allow(
        dead_code,
        reason = "foundation API for daily per-cause usage aggregation"
    )]
    pub const fn utc_day(&self) -> u64 {
        match self.at_unix_ms.checked_div(MS_PER_DAY) {
            Some(day) => day,
            None => 0,
        }
    }
}

/// Aggregated token counts and spend across model calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    dead_code,
    reason = "foundation API for daily per-cause usage aggregation"
)]
pub struct UsageTotals {
    /// Number of model calls counted.
    pub calls: u64,
    /// Total input tokens across calls.
    pub input_tokens: u64,
    /// Input tokens served from prompt cache.
    pub cached_tokens: u64,
    /// Input tokens written into prompt cache.
    pub cache_write_tokens: u64,
    /// Total output tokens across calls.
    pub output_tokens: u64,
    /// Output tokens spent reasoning.
    pub reasoning_tokens: u64,
    /// Total spend in millionths of the currency.
    pub cost_micros: u64,
}

#[allow(
    dead_code,
    reason = "foundation API for daily per-cause usage aggregation"
)]
impl UsageTotals {
    /// Add one [`UsageEntry`] into these totals.
    pub fn add(&mut self, entry: &UsageEntry) {
        self.calls = self.calls.saturating_add(1);
        self.cost_micros = self.cost_micros.saturating_add(entry.cost_micros);
        if let Some(usage) = entry.usage {
            self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
            self.cached_tokens = self
                .cached_tokens
                .saturating_add(usage.input_tokens_details.cached_tokens);
            self.cache_write_tokens = self
                .cache_write_tokens
                .saturating_add(usage.input_tokens_details.cache_write_tokens);
            self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
            self.reasoning_tokens = self
                .reasoning_tokens
                .saturating_add(usage.output_tokens_details.reasoning_tokens);
        }
    }

    /// Input tokens neither read from nor written to prompt cache.
    #[must_use]
    pub const fn uncached_input_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_sub(self.cached_tokens)
            .saturating_sub(self.cache_write_tokens)
    }
}

/// Key for daily totals grouped by cause kind and optional cause identifier.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(
    dead_code,
    reason = "foundation API for daily per-cause usage aggregation"
)]
pub struct CauseDayKey {
    /// Whole UTC days since the Unix epoch.
    pub utc_day: u64,
    /// Broad cause category.
    pub kind: CauseKind,
    /// Optional specific event, check, or task identifier.
    pub id: Option<String>,
}

/// Parse a newline-delimited usage log (`TILO_AGENT_USAGE_LOG`), skipping blank lines.
#[allow(
    dead_code,
    reason = "foundation API for daily per-cause usage aggregation"
)]
pub fn parse_usage_log(contents: &str) -> Result<Vec<UsageEntry>, serde_json::Error> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(serde_json::from_str)
        .collect()
}

/// Roll tokens and spend up per `(utc_day, CauseKind)`.
#[must_use]
#[allow(
    dead_code,
    reason = "foundation API for daily per-cause usage aggregation"
)]
pub fn aggregate_by_cause_and_day<'a>(
    entries: impl IntoIterator<Item = &'a UsageEntry>,
) -> BTreeMap<(u64, CauseKind), UsageTotals> {
    let mut totals: BTreeMap<(u64, CauseKind), UsageTotals> = BTreeMap::new();
    for entry in entries {
        totals
            .entry((entry.utc_day(), entry.cause.kind))
            .or_default()
            .add(entry);
    }
    totals
}

/// Roll tokens and spend up per `(utc_day, CauseKind, cause.id)`.
#[must_use]
#[allow(
    dead_code,
    reason = "foundation API for daily per-cause usage aggregation"
)]
pub fn aggregate_by_cause_id_and_day<'a>(
    entries: impl IntoIterator<Item = &'a UsageEntry>,
) -> BTreeMap<CauseDayKey, UsageTotals> {
    let mut totals: BTreeMap<CauseDayKey, UsageTotals> = BTreeMap::new();
    for entry in entries {
        let key = CauseDayKey {
            utc_day: entry.utc_day(),
            kind: entry.cause.kind,
            id: entry.cause.id.clone(),
        };
        totals.entry(key).or_default().add(entry);
    }
    totals
}

/// Build the [`UsageEntry`] appended for one model call.
fn usage_entry(
    (model, tier): (&str, Option<&str>),
    agent: Option<&str>,
    (elapsed, (pictures, picture_bytes)): (Duration, (usize, usize)),
    answer: &Result<ModelResponse, ModelError>,
    attempt: Attempt,
    cause: &CallCause,
) -> UsageEntry {
    let response = answer.as_ref().ok();
    UsageEntry {
        at_unix_ms: tilo_clock::unix_now_ms(),
        agent: agent.map(str::to_owned),
        model: model.to_owned(),
        elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        ok: answer.is_ok(),
        error: answer.as_ref().err().map(ToString::to_string),
        usage: response.map(|response| UsageCounts::from_protocol(&response.usage)),
        cost_micros: response.map_or(0, |response| response.cost_micros),
        cause: cause.clone(),
        stop_reason: response.map(|response| response.stop_reason),
        refusal: response.and_then(refusal),
        pictures,
        picture_bytes,
        hedge: attempt.hedge,
        retry: attempt.retry,
        service_tier: tier.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tilo_model_gateway::host_backend::protocol::{
        Message, ModelResponse, Role, StopReason, Usage,
    };

    use super::{
        Attempt, CallCause, CauseDayKey, CauseKind, GatewayTurns, MS_PER_DAY, PlanTurns,
        UsageEntry, aggregate_by_cause_and_day, aggregate_by_cause_id_and_day, append_usage_entry,
        current_call_cause, parse_usage_log, usage_entry, with_call_cause,
    };

    fn sample_response(
        input: u64,
        cached: u64,
        cache_write: u64,
        output: u64,
        reasoning: u64,
        cost_micros: u64,
    ) -> ModelResponse {
        ModelResponse {
            message: Message::text(Role::Assistant, "ok"),
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                total_tokens: input.saturating_add(output),
                cached_input_tokens: cached,
                cache_write_tokens: cache_write,
                reasoning_tokens: reasoning,
            },
            cost_micros,
        }
    }

    #[test]
    fn usage_entry_serializes_structured_cause_and_cost_and_round_trips() {
        let answer = Ok(sample_response(1_000, 600, 100, 250, 40, 1_500));
        let cause = CallCause::event("evt-42")
            .with_name("investor_mail")
            .with_task_id("task-7");
        let entry = usage_entry(
            ("claude-sonnet", Some("priority")),
            Some("assistant"),
            (Duration::from_millis(320), (1, 2_048)),
            &answer,
            Attempt {
                hedge: false,
                retry: true,
            },
            &cause,
        );

        let value = serde_json::to_value(&entry).expect("serialize usage entry");
        assert_eq!(value["model"], "claude-sonnet");
        assert_eq!(value["agent"], "assistant");
        assert_eq!(value["cost_micros"], 1_500);
        assert_eq!(value["service_tier"], "priority");
        assert_eq!(value["cause"]["kind"], "event");
        assert_eq!(value["cause"]["id"], "evt-42");
        assert_eq!(value["cause"]["name"], "investor_mail");
        assert_eq!(value["cause"]["task_id"], "task-7");
        assert_eq!(value["usage"]["input_tokens"], 1_000);
        assert_eq!(value["usage"]["input_tokens_details"]["cached_tokens"], 600);
        assert_eq!(
            value["usage"]["input_tokens_details"]["cache_write_tokens"],
            100
        );
        assert_eq!(value["usage"]["output_tokens"], 250);
        assert_eq!(
            value["usage"]["output_tokens_details"]["reasoning_tokens"],
            40
        );

        let decoded: UsageEntry =
            serde_json::from_value(value).expect("deserialize usage entry back");
        assert_eq!(decoded, entry);
    }

    #[test]
    fn legacy_log_lines_without_cause_or_cost_default_cleanly() {
        let legacy_missing = r#"{
            "at_unix_ms": 1700000000000,
            "agent": "assistant",
            "model": "gpt-5",
            "elapsed_ms": 410,
            "ok": true,
            "error": null,
            "usage": {
                "input_tokens": 500,
                "output_tokens": 100,
                "total_tokens": 600,
                "input_tokens_details": {"cached_tokens": 200, "cache_write_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": 10}
            },
            "stop_reason": "end_turn",
            "refusal": null,
            "pictures": 0,
            "picture_bytes": 0,
            "hedge": false,
            "retry": false
        }"#;
        let parsed: UsageEntry =
            serde_json::from_str(legacy_missing).expect("parse legacy line without cause");
        assert_eq!(parsed.cause, CallCause::turn());
        assert_eq!(parsed.cost_micros, 0);
        assert_eq!(parsed.service_tier, None);

        let legacy_null_cause =
            legacy_missing.replace("\"ok\": true,", "\"ok\": true, \"cause\": null,");
        let parsed_null: UsageEntry =
            serde_json::from_str(&legacy_null_cause).expect("parse line with null cause");
        assert_eq!(parsed_null.cause, CallCause::turn());
    }

    #[test]
    fn with_call_cause_scopes_nested_calls_and_restores_previous() {
        assert_eq!(current_call_cause(), CallCause::turn());
        with_call_cause(CallCause::look().with_name("morning_check"), || {
            assert_eq!(current_call_cause().kind, CauseKind::Look);
            assert_eq!(current_call_cause().name.as_deref(), Some("morning_check"));
            with_call_cause(CallCause::reflection().with_id("ref-1"), || {
                assert_eq!(current_call_cause().kind, CauseKind::Reflection);
                assert_eq!(current_call_cause().id.as_deref(), Some("ref-1"));
            });
            assert_eq!(current_call_cause().kind, CauseKind::Look);
        });
        assert_eq!(current_call_cause(), CallCause::turn());
    }

    #[test]
    fn gateway_turns_explicit_cause_overrides_thread_scope() {
        let default_turns = GatewayTurns {
            profile_id: "p1".into(),
            model_id: "m1".into(),
            sees: false,
            cause: None,
        };
        assert_eq!(default_turns.cause(), CallCause::turn());
        with_call_cause(CallCause::look(), || {
            assert_eq!(default_turns.cause(), CallCause::look());
        });

        let mut tagged = GatewayTurns {
            profile_id: "p1".into(),
            model_id: "m1".into(),
            sees: false,
            cause: None,
        }
        .with_cause(CallCause::event("evt-9"));
        with_call_cause(CallCause::look(), || {
            assert_eq!(tagged.cause(), CallCause::event("evt-9"));
        });
        tagged.set_cause(CallCause::reflection().with_id("ref-2"));
        assert_eq!(tagged.cause(), CallCause::reflection().with_id("ref-2"));
    }

    #[test]
    fn aggregates_tokens_and_spend_per_cause_and_day() {
        let day1 = 20_000_u64.saturating_mul(MS_PER_DAY);
        let day2 = 20_001_u64.saturating_mul(MS_PER_DAY);

        let mut e1 = usage_entry(
            ("m", None),
            None,
            (Duration::from_millis(10), (0, 0)),
            &Ok(sample_response(1_000, 400, 100, 200, 20, 500)),
            Attempt::default(),
            &CallCause::turn(),
        );
        e1.at_unix_ms = day1.saturating_add(1_000);

        let mut e2 = usage_entry(
            ("m", None),
            None,
            (Duration::from_millis(10), (0, 0)),
            &Ok(sample_response(2_000, 1_500, 0, 300, 50, 700)),
            Attempt::default(),
            &CallCause::event("evt-a").with_name("mail"),
        );
        e2.at_unix_ms = day1.saturating_add(5_000);

        let mut e3 = usage_entry(
            ("m", None),
            None,
            (Duration::from_millis(10), (0, 0)),
            &Ok(sample_response(800, 200, 50, 100, 0, 300)),
            Attempt::default(),
            &CallCause::event("evt-b"),
        );
        e3.at_unix_ms = day1.saturating_add(9_000);

        let mut e4 = usage_entry(
            ("m", None),
            None,
            (Duration::from_millis(10), (0, 0)),
            &Ok(sample_response(600, 0, 0, 80, 0, 150)),
            Attempt::default(),
            &CallCause::look(),
        );
        e4.at_unix_ms = day2.saturating_add(500);

        let mut e5 = usage_entry(
            ("m", None),
            None,
            (Duration::from_millis(10), (0, 0)),
            &Ok(sample_response(900, 300, 0, 120, 30, 250)),
            Attempt::default(),
            &CallCause::reflection().with_id("daily"),
        );
        e5.at_unix_ms = day2.saturating_add(1_500);

        // Write to a temporary JSONL file and reload via `parse_usage_log`.
        let file = tempfile::NamedTempFile::new().expect("temp usage log");
        for entry in [&e1, &e2, &e3, &e4, &e5] {
            append_usage_entry(file.path(), entry).expect("append entry");
        }
        let raw = std::fs::read_to_string(file.path()).expect("read usage log");
        let entries = parse_usage_log(&raw).expect("parse jsonl usage log");
        assert_eq!(entries.len(), 5);

        let by_kind = aggregate_by_cause_and_day(&entries);
        let day1_turn = by_kind[&(20_000, CauseKind::Turn)];
        assert_eq!(day1_turn.calls, 1);
        assert_eq!(day1_turn.input_tokens, 1_000);
        assert_eq!(day1_turn.cached_tokens, 400);
        assert_eq!(day1_turn.cache_write_tokens, 100);
        assert_eq!(day1_turn.uncached_input_tokens(), 500);
        assert_eq!(day1_turn.output_tokens, 200);
        assert_eq!(day1_turn.reasoning_tokens, 20);
        assert_eq!(day1_turn.cost_micros, 500);

        let day1_events = by_kind[&(20_000, CauseKind::Event)];
        assert_eq!(day1_events.calls, 2);
        assert_eq!(day1_events.input_tokens, 2_800);
        assert_eq!(day1_events.cached_tokens, 1_700);
        assert_eq!(day1_events.cache_write_tokens, 50);
        assert_eq!(day1_events.uncached_input_tokens(), 1_050);
        assert_eq!(day1_events.output_tokens, 400);
        assert_eq!(day1_events.cost_micros, 1_000);

        let day2_look = by_kind[&(20_001, CauseKind::Look)];
        assert_eq!(day2_look.calls, 1);
        assert_eq!(day2_look.cost_micros, 150);

        let day2_reflection = by_kind[&(20_001, CauseKind::Reflection)];
        assert_eq!(day2_reflection.calls, 1);
        assert_eq!(day2_reflection.cost_micros, 250);

        let by_id = aggregate_by_cause_id_and_day(&entries);
        assert_eq!(
            by_id[&CauseDayKey {
                utc_day: 20_000,
                kind: CauseKind::Event,
                id: Some("evt-a".into()),
            }]
                .cost_micros,
            700
        );
        assert_eq!(
            by_id[&CauseDayKey {
                utc_day: 20_000,
                kind: CauseKind::Event,
                id: Some("evt-b".into()),
            }]
                .cost_micros,
            300
        );
    }
}
