//! Running a shortcut through an [`ActionHost`].
//!
//! The interpreter never acts in its own name. Every step is an invocation by
//! the [`Caller`] who started the run, so the host applies that person's own
//! authority; before each one the interpreter also requires the action to lie
//! inside the shortcut's [`grant`](Shortcut::grant). The run can therefore do
//! no more than the person could and no more than the shortcut was allowed.
//!
//! Each invocation id is derived from the run id and the step's path
//! (including the loop iteration), so running the same [`RunId`] again — a
//! retry after a crash or a lost reply — first observes what the host already
//! did and reuses the same id for anything it re-sends, instead of acting
//! twice.
//!
//! The interpreter is `async` in the same runtime-free way [`Pending`] is:
//! the future it returns can be awaited by any executor, or polled from a
//! frame loop. Waiting for a background action and anything that needs a
//! person — a confirmation, an [`Ask`](StepKind::Ask) — go through the
//! caller's [`RunHooks`].

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tilo_actions::ActionKey;
use tilo_actions::context::{ActionContext, ActionFacet};
use tilo_actions::contract::{ConfirmationPolicy, ResultSpec};
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_actions::outcome::{ActionFailure, ActionOutput, InvocationId};
use tilo_actions::params::{ActionArgs, ActionParam, ParamType, validate_json_schema};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CatalogueQuery, CatalogueRow, Confirmation,
    InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE, Pending, ReceiptStatus,
};
use uuid::Uuid;

use crate::StepPolicy;
use crate::catalog::{ResourceTarget, find_row};
use crate::grant::{
    EDIT_PARAM, FIELD_EDIT, StepGrant, edit_reach, grant_answer, step_node, step_node_at,
};
use crate::path::StepPath;
use crate::shortcut::{AiTransform, Binding, Condition, Selector, Shortcut, Step, StepKind};
use crate::template::{self, Piece, render_value};
use crate::{
    AGENT_TRANSFORM_KIND, SHORTCUT_RUN_ACTION, SHORTCUTS_APP_ID, TriggerSpec, shortcut_id_of,
};
use tilo_action_policy::Answer;

/// How many catalogue pages the interpreter reads looking for one action.
const MAX_CATALOGUE_PAGES: usize = 32;

/// What a secret looks like in a run report or a confirmation prompt.
const REDACTED: &str = "[redacted]";

/// Every [`StepFailure::code`] the run itself gives a failed step, in the
/// order a person reads them. A failure of an action carries the action's
/// own code instead; these are the rest.
pub const STEP_FAILURE_CODES: &[&str] = &[
    "inputs",
    "binding",
    "unresolved",
    "outside_grant",
    "asks_first",
    "unavailable",
    "confirmation_required",
    "declined",
    "needs_person",
    "recursion",
    "invalid_arguments",
    "remote_actions_unavailable",
    "unknown_action",
    "not_permitted",
    "action_confirmation_required",
    "stale_revision",
    "unknown_invocation",
    "invocation_conflict",
    "not_cancellable",
    "refused",
    "cancelled",
    "contract",
    "abandoned",
    "stopped",
    "timeout",
];

/// The longest wait between two attempts of a retried step.
pub const MAX_BACKOFF_MS: u64 = 600_000;
/// The wait a [`Retry`](StepPolicy::Retry) names when it names none: the
/// whole step's patience, at most, so a retry never outlasts it.
pub const DEFAULT_BACKOFF_MS: u64 = 60_000;

/// Identity of one run. Reusing it reuses every invocation id the run
/// derives, which is what makes re-running it safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RunId(pub Uuid);

impl RunId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

/// The facet naming the step path of a run's change.
pub const STEP_FACET: &str = "step";

#[must_use]
pub fn invocation_id(run: RunId, path: &StepPath) -> InvocationId {
    let mut hasher = Sha256::new();
    hasher.update(b"tilo.automation.invocation\0");
    hasher.update(run.0.as_bytes());
    hasher.update(path.as_str().as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    for (byte, digested) in bytes.iter_mut().zip(digest.iter()) {
        *byte = *digested;
    }
    InvocationId(uuid::Builder::from_custom_bytes(bytes).into_uuid())
}

/// Whether a person is at hand to answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// Started by a person who can confirm actions and answer questions.
    Interactive,
    /// Started by a trigger. An action that needs confirmation, or a step
    /// that asks, stops the run instead.
    Unattended,
}

/// What started a run.
#[derive(Clone, Debug, PartialEq)]
pub enum RunTrigger {
    /// A person ran it.
    Manual,
    Fired(TriggerSpec),
}

impl RunTrigger {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Fired(trigger) => trigger.kind(),
        }
    }
}

/// Everything one run starts from.
#[derive(Clone, Debug)]
pub struct RunRequest<'a> {
    pub shortcut: &'a Shortcut,
    pub run: RunId,
    /// The authenticated principal who started the run; every step is
    /// invoked as them.
    pub caller: Caller,
    pub mode: RunMode,
    pub trigger: RunTrigger,
    /// Values for the shortcut's declared inputs.
    pub inputs: Map<String, Value>,
    /// The bot run this run continues, recorded on every change it makes.
    pub caused_by: Option<tilo_actions::context::Cause>,
    /// The steps an earlier run recorded, when this run continues that one.
    /// See [`resumed`].
    pub resume: Option<&'a RunReport>,
    /// The step the run continues from, when it continues an earlier one.
    pub from: Option<StepPath>,
    /// The work unit this run belongs to when something else asked for it (a
    /// task that called the shortcut), named by its own group. Every step is
    /// stamped with it instead of this run's own group, so the run reads as
    /// part of what asked for it and is undone with it.
    pub parent: Option<tilo_actions::context::ActionGroup>,
    /// The shortcuts already running above this one, outermost first: its own
    /// id when it is a triggered run, or the other shortcuts that called it.
    /// A step that would run one of them is not run, so two shortcuts that
    /// call each other spin once and stop.
    pub chain: Vec<String>,
}

/// Which run a run goes on from, and from where: what a person or an agent
/// asks for when they fix a step of a shortcut that failed and run it again
/// without repeating the steps before it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Continuation {
    /// The run that happened.
    pub run: RunId,
    /// The step to go on from: it runs again, and so does everything after
    /// it. The steps before it are taken from the record as they happened.
    pub from: StepPath,
}

/// The steps a continued run takes from the record instead of producing
/// again: the recorded steps of the run it continues that come before
/// `from`, and nothing else.
///
/// A report records its steps in the order they ran, so the steps before
/// `from` are the prefix up to the first record at `from` or below it. Those
/// steps' output — looked up on the host when a later step reads it — is what
/// the host already did, under the invocations it already answered. `from`
/// itself and everything after it are not in this set: they run again from
/// the shortcut as it is now, which is how a fixed step takes effect.
///
/// A run with no origin replays nothing.
fn resumed<'r>(origin: Option<&'r RunReport>, from: Option<&StepPath>) -> Vec<&'r StepRecord> {
    let (Some(origin), Some(from)) = (origin, from) else {
        return Vec::new();
    };
    origin
        .steps
        .iter()
        .take_while(|record| &record.path != from && !record.path.is_inside(from))
        .collect()
}

/// An action that needs the person's confirmation before it runs.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfirmationRequest {
    pub path: StepPath,
    pub step: String,
    pub app_id: String,
    pub descriptor: ActionDescriptor,
    pub policy: ConfirmationPolicy,
    /// The bound arguments, with secret parameters redacted.
    pub arguments: Map<String, Value>,
}

/// A question an [`Ask`](StepKind::Ask) step puts to the person.
#[derive(Clone, Debug, PartialEq)]
pub struct AskRequest {
    pub path: StepPath,
    pub step: String,
    pub param: ActionParam,
}

/// How a run reaches its caller while it is running.
pub trait RunHooks: Sync {
    /// Ask the person to confirm an action; `None` declines it. Only called
    /// in an [`Interactive`](RunMode::Interactive) run.
    fn confirm(&self, request: ConfirmationRequest) -> Pending<Option<Confirmation>>;

    /// Ask the person for a value; `None` declines. Only called in an
    /// [`Interactive`](RunMode::Interactive) run.
    fn ask(&self, request: AskRequest) -> Pending<Option<Value>>;

    /// Resolve when the running invocation `invocation` is worth observing
    /// again — after a poll interval. Only called for a host whose
    /// [`ActionHost::next_change`]
    /// cannot tell when the invocation changes. Abandoning the answer stops
    /// the run.
    fn settle(&self, invocation: InvocationId) -> Pending<()>;

    /// Resolve once `what` has been asked, and every run in this process has
    /// stopped (`Err`), so the run stops too.
    ///
    /// The interpreter holds a step that is waiting; it is called before the
    /// step runs and before every look at a running invocation, so a stop
    /// takes effect before the next step is started, while an action already
    /// under way finishes and its effect is recorded as it is. A hook whose
    /// runs cannot be stopped outside their own questions need not answer;
    /// the default never stops one.
    fn stopped(&self, _what: StopWatch<'_>) -> Pending<Result<(), String>> {
        Pending::ready(Ok(()))
    }
}

/// What a run was doing when [`RunHooks::stopped`] was asked: the step it is
/// at, and when it got there.
#[derive(Clone, Copy, Debug)]
pub struct StopWatch<'a> {
    /// Where the step is; [`StepPath::root`] before the first one and at the
    /// shortcut's output binding.
    pub path: &'a StepPath,
    /// When the run reached this step.
    pub since: std::time::Instant,
}

impl StopWatch<'_> {
    /// How long the step has been running.
    #[must_use]
    pub fn elapsed(&self) -> std::time::Duration {
        self.since.elapsed()
    }
}

/// Why a step did not complete. A step's [`StepPolicy`] decides what the
/// run does about it; with none, the run stops at the step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "failure", rename_all = "snake_case")]
pub enum StepFailure {
    #[error("the run's inputs are invalid: {reason}")]
    Inputs { reason: String },
    #[error("{reason}")]
    Binding { reason: String },
    #[error("`{app_id}` offers no action `{action}` there: {reason}")]
    Unresolved {
        app_id: String,
        action: String,
        reason: String,
    },
    #[error("the shortcut's grant does not cover `{action}` on `{app_id}`")]
    OutsideGrant { app_id: String, action: String },
    #[error(
        "the shortcut's grant asks before `{action}` on `{app_id}`, and nobody is present to answer"
    )]
    AsksFirst { app_id: String, action: String },
    #[error("the action is unavailable: {reason}")]
    Unavailable { code: String, reason: String },
    #[error("the action needs confirmation, and nobody is present to give it")]
    ConfirmationRequired,
    #[error("the person declined")]
    Declined,
    #[error("the step asks a question, and nobody is present to answer it")]
    NeedsPerson,
    #[error("a shortcut may not run itself")]
    Recursion,
    #[error("invalid arguments: {reason}")]
    Arguments { reason: String },
    #[error("the host refused: {error}")]
    // Boxed: the host's and the action's errors carry JSON details, whose size
    // depends on the serde_json features the rest of the build enables.
    Host { error: Box<ActionHostError> },
    #[error("the action failed: {}", error.message)]
    Action { error: Box<ActionFailure> },
    #[error("the action was cancelled")]
    Cancelled { message: Option<String> },
    #[error("the action broke its declared contract: {reason}")]
    Contract { reason: String },
    #[error("the answer never came")]
    Abandoned,
    #[error("the run was stopped: {reason}")]
    Stopped { reason: String },
    #[error("the step did not finish within {seconds} s")]
    TimedOut { seconds: u64 },
}

impl StepFailure {
    /// Whether a run may go on past the step at all. A question nobody is
    /// there to answer, or a person's refusal, is not a step that fell
    /// short: passing it over would decide something for them. Everything
    /// else a run can carry on past, visibly, when the shortcut says how.
    #[must_use]
    pub const fn ends_run(&self) -> bool {
        matches!(
            self,
            Self::NeedsPerson
                | Self::Declined
                | Self::Arguments { .. }
                | Self::Inputs { .. }
                | Self::Recursion
                | Self::ConfirmationRequired
        )
    }

    /// Whether trying the step again at the same moment could go
    /// differently without applying anything twice: the host or the service
    /// was unavailable, an action said it was retryable, or the wait for it
    /// ran out with the invocation still under way (a retry picks up that
    /// same invocation instead of starting it again).
    #[must_use]
    pub fn transient(&self) -> bool {
        match self {
            Self::Unavailable { .. } | Self::TimedOut { .. } => true,
            Self::Host { error } => matches!(**error, ActionHostError::Unavailable { .. }),
            Self::Action { error } => error.retryable,
            _ => false,
        }
    }

    /// The failure as a branch or a person matches on it: one of
    /// `STEP_FAILURE_CODES`, and the action's
    /// own code when the action is what failed.
    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Inputs { .. } => "inputs",
            Self::Binding { .. } => "binding",
            Self::Unresolved { .. } => "unresolved",
            Self::OutsideGrant { .. } => "outside_grant",
            Self::AsksFirst { .. } => "asks_first",
            Self::Unavailable { code, .. } => code,
            Self::ConfirmationRequired => "confirmation_required",
            Self::Declined => "declined",
            Self::NeedsPerson => "needs_person",
            Self::Recursion => "recursion",
            Self::Arguments { .. } => "invalid_arguments",
            Self::Host { error } => error.code(),
            Self::Action { error } => &error.code,
            Self::Cancelled { .. } => "cancelled",
            Self::Contract { .. } => "contract",
            Self::Abandoned => "abandoned",
            Self::Stopped { .. } => "stopped",
            Self::TimedOut { .. } => "timeout",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepStatus {
    Succeeded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
    },
    /// The step did not succeed. Every failure says which try it was and
    /// why; the run may have carried on from here, and `recovered` says how.
    Failed {
        failure: StepFailure,
        /// 1 for the first try; more when the step was retried.
        #[serde(default = "one", skip_serializing_if = "is_first_attempt")]
        attempt: u32,
        #[serde(default, skip_serializing_if = "Recovered::is_stopped")]
        recovered: Recovered,
    },
}

const fn one() -> u32 {
    1
}

/// `serde`'s predicate for a field it may leave out, so it takes the field
/// by reference; one attempt is the value an absent field reads as.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde calls a skip predicate with the field by reference"
)]
fn is_first_attempt(attempt: &u32) -> bool {
    *attempt == 1
}

/// What the run did about a step that failed. Only [`Recovered::Stopped`]
/// leaves the run to end there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "recovered", rename_all = "snake_case")]
pub enum Recovered {
    /// The run stopped at the step.
    #[default]
    Stopped,
    /// The next try succeeded.
    Retried,
    /// A fallback ran in the step's place.
    Fallback,
    /// The step's declared default stood in for its output. The run is
    /// degraded: what it produced rests on a value the step did not.
    Default,
}

impl Recovered {
    /// Whether the run ends at the step.
    #[must_use]
    pub const fn is_stopped(&self) -> bool {
        matches!(self, Self::Stopped)
    }
}

impl StepStatus {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Succeeded { .. })
    }

    /// Why the step failed, the try it was and what the run did about it.
    #[must_use]
    pub fn failure(&self) -> Option<(&StepFailure, u32, &Recovered)> {
        match self {
            Self::Succeeded { .. } => None,
            Self::Failed {
                failure,
                attempt,
                recovered,
            } => Some((failure, *attempt, recovered)),
        }
    }
}

/// One step as it ran, in order. A loop body's steps appear once per
/// iteration, each with its own path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub path: StepPath,
    pub step: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<InvocationId>,
    /// Every invocation a retried step sent, in the order it sent them. One
    /// entry for a step that was not retried.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempts: Vec<InvocationId>,
    /// A step that did not succeed, with what the run did about it.
    #[serde(default, skip_serializing_if = "StepOutcome::is_none")]
    pub degraded: StepOutcome,
    #[serde(flatten)]
    pub status: StepStatus,
}

impl StepRecord {
    /// This step as a continued run takes it: what it did happened once, so
    /// the invocation is the one it ran under and what it produced is what
    /// the record shows.
    ///
    /// A step that answered a secret question shows nothing, so nothing is
    /// what a later step is given from here; the invocation still holds the
    /// value for the host to answer with (see [`Runner::replayed_value`]).
    #[must_use]
    fn taken(&self) -> Done {
        Done {
            invocation: self.invocation,
            attempts: Vec::new(),
            retryable: false,
            recorded: false,
            result: Ok(match &self.status {
                StepStatus::Succeeded { output } => output.clone(),
                StepStatus::Failed { .. } => None,
            }),
        }
    }
}

/// How a step left the run: not at all, or short of what it promised.
///
/// A step the run carried on past is here whether it failed and was
/// retried, ran a fallback, or stood on its declared default value — a
/// failure is never skipped quietly.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum StepOutcome {
    /// The step did what it was there for.
    #[default]
    None,
    /// The step failed, and the run went on: [`Recovered`] says how.
    Degraded {
        failure: StepFailure,
        #[serde(default = "one", skip_serializing_if = "is_first_attempt")]
        attempt: u32,
        recovered: Recovered,
    },
}

impl StepOutcome {
    #[must_use]
    pub const fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// The failure as a code and in words, for a branch to match and a chip
    /// to show: "timeout", "the step did not finish within 60 s".
    #[must_use]
    pub fn described(&self) -> (&str, String) {
        match self {
            Self::None => ("", String::new()),
            Self::Degraded { failure, .. } => (failure.code(), failure.to_string()),
        }
    }

    #[must_use]
    pub const fn degraded(failure: StepFailure, attempt: u32, recovered: Recovered) -> Self {
        Self::Degraded {
            failure,
            attempt,
            recovered,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RunOutcome {
    Completed,
    /// A step failed; nothing after it ran.
    Failed {
        path: StepPath,
        failure: StepFailure,
    },
    /// The run was stopped before the step at `at`: what it had already done
    /// stands, and can be undone as one step like a completed run's.
    Cancelled {
        at: StepPath,
    },
}

/// The history entry of one run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub run: RunId,
    pub shortcut: String,
    /// BLAKE3 hex of the version of the shortcut that ran
    /// ([`program_hash`](crate::program::program_hash)), so the record of a
    /// run says which program produced it after the shortcut is edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The trigger kind (`manual`, `schedule`, …).
    pub trigger: String,
    pub mode: RunMode,
    pub steps: Vec<StepRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// The steps that fell short and what the run did about them, in run
    /// order. A run with any is degraded: what it reports may rest on a
    /// value some step did not produce.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<StepOutcome>,
    #[serde(flatten)]
    pub outcome: RunOutcome,
}

impl RunReport {
    /// What the run reports, with a note saying which of its steps fell
    /// short when any did. A degraded run really did finish, so its caller
    /// needs both: the value it produced and the fact that value may rest on
    /// something a step did not deliver. A run that finished as intended
    /// reports its value as it is.
    #[must_use]
    pub fn reported_value(&self) -> Option<Value> {
        let Some(summary) = self.degraded_summary() else {
            return self.output.clone();
        };
        let mut reported = serde_json::Map::new();
        if let Some(output) = &self.output {
            reported.insert("output".to_string(), output.clone());
        }
        reported.insert("degraded".to_string(), Value::String(summary));
        Some(Value::Object(reported))
    }

    /// Every invocation id the run used, in order.
    #[must_use]
    pub fn invocations(&self) -> Vec<InvocationId> {
        self.steps
            .iter()
            .filter_map(|step| step.invocation)
            .collect()
    }

    /// Every invocation id the run sent, retries included, in order.
    #[must_use]
    pub fn sent(&self) -> Vec<InvocationId> {
        self.steps
            .iter()
            .flat_map(|step| {
                if step.attempts.is_empty() {
                    step.invocation.into_iter().collect()
                } else {
                    step.attempts.clone()
                }
            })
            .collect()
    }

    /// Whether any step fell short and the run went on anyway.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        !self.degraded.is_empty()
    }

    /// What to say about a degraded run, or `None` when nothing fell short.
    #[must_use]
    pub fn degraded_summary(&self) -> Option<String> {
        let (_, first) = self.degraded.first()?.described();
        let count = self.degraded.len();
        let steps = if count == 1 {
            "1 step".to_string()
        } else {
            format!("{count} steps")
        };
        if count == 1 {
            Some(format!("{steps} did not finish as intended: {first}"))
        } else {
            Some(format!(
                "{steps} did not finish as intended, the first: {first}"
            ))
        }
    }
}

/// Run `request.shortcut` on `host`.
///
/// Stops at the first step that fails; the report says which and why.
pub async fn run<H: ActionHost + Sync, K: RunHooks>(
    host: &H,
    hooks: &K,
    request: RunRequest<'_>,
) -> RunReport {
    let RunRequest {
        shortcut,
        run,
        caller,
        mode,
        trigger,
        inputs,
        caused_by,
        parent,
        chain,
        ..
    } = request;
    let resumed = resumed(request.resume, request.from.as_ref());
    let mut report = RunReport {
        run,
        shortcut: shortcut.id.clone(),
        version: crate::program::canonical_bytes(shortcut)
            .ok()
            .map(|bytes| crate::program::program_hash(&bytes)),
        trigger: trigger.kind().to_string(),
        mode,
        steps: Vec::new(),
        output: None,
        degraded: Vec::new(),
        outcome: RunOutcome::Completed,
    };
    let inputs = match ActionArgs::bind_json(&shortcut.inputs, &inputs) {
        Ok(bound) => bound.to_json(),
        Err(error) => {
            report.outcome = RunOutcome::Failed {
                path: StepPath::root(),
                failure: StepFailure::Inputs {
                    reason: error.to_string(),
                },
            };
            return report;
        }
    };
    let mut runner = Runner {
        host,
        hooks,
        shortcut,
        run,
        caller,
        mode,
        trigger: report.trigger.clone(),
        inputs,
        vars: BTreeMap::new(),
        secret_vars: BTreeSet::new(),
        outputs: BTreeMap::new(),
        records: Vec::new(),
        resumed,
        caused_by,
        parent,
        chain,
        timeout: shortcut
            .step_timeout_secs
            .map_or(DEFAULT_STEP_TIMEOUT, |secs| {
                std::time::Duration::from_secs(secs.max(1))
            }),
        stopped: None,
        degraded: Vec::new(),
        failed: BTreeMap::new(),
    };
    let finished = runner.block(&shortcut.steps, StepPath::root()).await;
    let finished = finished.and_then(|()| match &shortcut.output {
        Some(output) => runner
            .evaluate(output)
            .map(|value| report.output = value)
            .map_err(|failure| (StepPath::output(), failure)),
        None => Ok(()),
    });
    if let Err((path, failure)) = finished {
        report.outcome = match runner.stopped.clone() {
            Some(at) => RunOutcome::Cancelled { at },
            None => RunOutcome::Failed { path, failure },
        };
    }
    report.steps = runner.records;
    report.degraded = runner.degraded;
    report
}

/// How long a step may run before the run stops waiting for it, when the
/// shortcut names no [`Shortcut::step_timeout_secs`].
pub const DEFAULT_STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

type Stop = (StepPath, StepFailure);
type Block<'s> = Pin<Box<dyn Future<Output = Result<(), Stop>> + Send + 's>>;

struct Runner<'a, H, K> {
    host: &'a H,
    hooks: &'a K,
    shortcut: &'a Shortcut,
    run: RunId,
    caller: Caller,
    mode: RunMode,
    trigger: String,
    inputs: Map<String, Value>,
    vars: BTreeMap<String, Value>,
    secret_vars: BTreeSet<String>,
    outputs: BTreeMap<String, Value>,
    records: Vec<StepRecord>,
    /// The steps an earlier run recorded, when this run continues it: their
    /// output is taken as it was instead of being produced again.
    resumed: Vec<&'a StepRecord>,
    caused_by: Option<tilo_actions::context::Cause>,
    parent: Option<tilo_actions::context::ActionGroup>,
    /// The shortcuts running above this one (see [`RunRequest::chain`]).
    chain: Vec<String>,
    /// How long each step of the run may take.
    timeout: std::time::Duration,
    /// The steps that failed and the run carried on past, with why.
    degraded: Vec<StepOutcome>,
    /// The step the run was told to stop at, when it was.
    stopped: Option<StepPath>,
    /// The steps that already failed, by id, and whether their output can
    /// stand as no value. A later step reading one of them is not skipped
    /// quietly: it runs, and fails saying which step it depends on.
    failed: BTreeMap<String, FailedStep>,
}

/// A step that did not succeed, as later steps see it.
#[derive(Clone, Debug)]
struct FailedStep {
    /// What the step failed with.
    failure: StepFailure,
    /// Whether reading it as no value is honest: a step that failed before
    /// it sent anything, or whose only effect is a resource or a job, which
    /// it did not produce. A step that may have changed something keeps
    /// `false`: its effect stands, and a later step resting on it would be
    /// resting on a change nobody asked for.
    output_is_none: bool,
    /// The recovery the run already took for it, so a dependent step can
    /// repeat it.
    recovered: Recovered,
}

/// A step's result before it is recorded.
struct Done {
    invocation: Option<InvocationId>,
    /// Every invocation the step sent, retries included.
    attempts: Vec<InvocationId>,
    /// Whether the step is one that can be tried again at all.
    retryable: bool,
    /// The step already has its record: an `if` or a loop writes it before
    /// its body runs, so its body's failure is the step's failure.
    recorded: bool,
    result: Result<Option<Value>, StepFailure>,
}

impl Done {
    fn local(result: Result<Option<Value>, StepFailure>) -> Self {
        Self {
            invocation: None,
            attempts: Vec::new(),
            retryable: false,
            recorded: false,
            result,
        }
    }

    /// A step whose record its own work already wrote.
    fn recorded() -> Self {
        Self {
            recorded: true,
            ..Self::local(Ok(None))
        }
    }
}

/// How often a wait looks again while its other answers are still pending.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Whether running `kind` invokes a published action — the steps a stop must
/// keep from starting, whose effect would otherwise land.
fn invokes(kind: &StepKind) -> bool {
    matches!(
        kind,
        StepKind::Invoke { .. } | StepKind::Ai { .. } | StepKind::Task { .. }
    )
}

/// Whether a step of this kind leaves nothing behind when it fails: what a
/// failing agent transform reports is not a resource, and the step that
/// failed produced no value at all.
fn produces_no_resource(kind: &StepKind) -> bool {
    matches!(kind, StepKind::Ai { .. } | StepKind::Task { .. })
}

/// Every step id `step` reads: a value bound to a step's output names it,
/// whether whole or by field.
fn reads(step: &Step) -> Vec<String> {
    let mut out = Vec::new();
    let mut binding = |b: &Binding| collect_reads(b, &mut out);
    match &step.kind {
        StepKind::Invoke { resource, args, .. } => {
            if let Selector::FromBinding(selected) = resource {
                binding(selected);
            }
            args.values().for_each(&mut binding);
        }
        StepKind::If {
            cond,
            then,
            otherwise,
        } => {
            let mut condition = |c: &Condition| collect_condition_reads(c, &mut out);
            condition(cond);
            let mut body = |steps: &[Step]| {
                for inner in steps {
                    out.extend(reads(inner));
                }
            };
            body(then);
            body(otherwise);
        }
        StepKind::ForEach { items, body, .. } => {
            binding(items);
            for inner in body {
                out.extend(reads(inner));
            }
        }
        StepKind::SetVar { value, .. } => binding(value),
        StepKind::Ai { input, .. } => input.as_ref().into_iter().for_each(&mut binding),
        StepKind::Ask { .. } | StepKind::Task { .. } => {}
    }
    if let StepPolicy::Fallback { steps } = &step.on_error {
        for inner in steps {
            out.extend(reads(inner));
        }
    }
    out
}

fn collect_reads(binding: &Binding, out: &mut Vec<String>) {
    match binding {
        Binding::StepOutput { step, .. } => out.push(step.clone()),
        Binding::Object(members) => members.values().for_each(|inner| collect_reads(inner, out)),
        Binding::List(items) => items.iter().for_each(|inner| collect_reads(inner, out)),
        Binding::Template(text) => {
            if let Ok(pieces) = crate::template::parse(text) {
                for piece in pieces {
                    if let crate::template::Piece::Value(inner) = piece {
                        collect_reads(&inner, out);
                    }
                }
            }
        }
        Binding::Literal(_) | Binding::Input(_) | Binding::Var(_) => {}
    }
}

fn collect_condition_reads(condition: &Condition, out: &mut Vec<String>) {
    match condition {
        Condition::Truthy(value) => collect_reads(value, out),
        Condition::Equals { left, right } => {
            collect_reads(left, out);
            collect_reads(right, out);
        }
        Condition::Not(inner) => collect_condition_reads(inner, out),
        Condition::All(all) | Condition::Any(all) => {
            for inner in all {
                collect_condition_reads(inner, out);
            }
        }
    }
}

/// Which of the waits a step is in answered first.
enum Wait {
    /// The host said the invocation may have moved.
    Moved(Result<(), StepFailure>),
    /// The caller stopped the run.
    Stopped(String),
    /// The step outstayed its patience.
    TimedOut,
}

/// Poll `moved`, `stopped` and the clock `left` until one of them answers
/// that the wait is over.
///
/// A future that already answered is not polled again — a [`Pending`] may be
/// polled any number of times, and polling a settled one reads as abandoned.
/// Looking at the move first keeps a host that pushes changes on the fast
/// path: its answer ends the wait. A host that cannot tell falls back to
/// [`POLL_INTERVAL`] while `refreshing` says so, and every look at the clock
/// asks the stop first, so a long wait still ends as soon as someone stops
/// it.
async fn wait_for<M, S>(moved: M, stopped: S, left: std::time::Duration, refreshing: bool) -> Wait
where
    M: Future<Output = Result<(), StepFailure>>,
    S: Future<Output = Option<Result<(), String>>>,
{
    let mut moved = std::pin::pin!(moved);
    let mut stopped = std::pin::pin!(stopped);
    let mut moved_done = false;
    let mut stopped_done = false;
    let mut left = left;
    loop {
        let mut clock = std::pin::pin!(WaitUntil(std::time::Instant::now().checked_add(left)));
        let outcome = std::future::poll_fn(|waker| {
            if !moved_done && let Poll::Ready(outcome) = moved.as_mut().poll(waker) {
                return Poll::Ready(match outcome {
                    Ok(()) => Wait::Moved(Ok(())),
                    Err(failure) => Wait::Moved(Err(failure)),
                });
            }
            if !stopped_done {
                match stopped.as_mut().poll(waker) {
                    Poll::Ready(Some(Err(reason))) => return Poll::Ready(Wait::Stopped(reason)),
                    // Nothing stopped the run: only the move is left to wait
                    // for, and the clock bounds it.
                    Poll::Ready(Some(Ok(()))) | Poll::Pending => {}
                    // The hook gave no answer: it cannot stop this run.
                    Poll::Ready(None) => stopped_done = true,
                }
            }
            if clock.as_mut().poll(waker).is_ready() {
                return Poll::Ready(Wait::TimedOut);
            }
            Poll::Pending
        })
        .await;
        match outcome {
            // The host pushed a change: the step looks at its invocation.
            Wait::Moved(Ok(())) => return Wait::Moved(Ok(())),
            Wait::Moved(Err(failure)) => return Wait::Moved(Err(failure)),
            Wait::Stopped(reason) => return Wait::Stopped(reason),
            // A host that cannot tell when an invocation moves falls back to
            // a poll interval: the move is tried again before the wait ends.
            Wait::TimedOut if refreshing && !moved_done => {
                moved_done = true;
                left = left.saturating_sub(POLL_INTERVAL);
            }
            Wait::TimedOut => return Wait::TimedOut,
        }
    }
}

/// Resolve once `at` has passed, on the waker that looks at it: the timer
/// belongs to whoever waits, so the interpreter needs none of its own.
/// `None` is a deadline already gone.
struct WaitUntil(Option<std::time::Instant>);

impl Future for WaitUntil {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        match self.0 {
            Some(at) if std::time::Instant::now() >= at => std::task::Poll::Ready(()),
            _ => std::task::Poll::Pending,
        }
    }
}

impl<'a, H: ActionHost + Sync, K: RunHooks> Runner<'a, H, K> {
    fn block<'s>(&'s mut self, steps: &'a [Step], path: StepPath) -> Block<'s>
    where
        'a: 's,
    {
        Box::pin(async move {
            for (index, step) in steps.iter().enumerate() {
                self.settled_step(step, path.child(index)).await?;
            }
            Ok(())
        })
    }

    /// The step at `path` as an earlier run that already took it left it:
    /// what it did happened once, and this run goes on from what it left.
    /// Only a completed step is taken as it was — a failed one is the step
    /// the person came to fix, and runs again.
    fn continues(&self, path: &StepPath, id: &str) -> Option<Done> {
        self.resumed
            .iter()
            .copied()
            .find(|record| {
                &record.path == path
                    && record.step == id
                    && matches!(record.status, StepStatus::Succeeded { .. })
            })
            .map(StepRecord::taken)
    }

    /// A loop an earlier run already ran: walk the iterations it recorded,
    /// so each iteration's body is taken from the record exactly as
    /// [`Runner::step`] reads a step, and a body the record has nothing for
    /// runs again. `items` is evaluated only when an iteration is replayed,
    /// to bind `item` as it was.
    async fn replayed(
        &mut self,
        items: &Binding,
        item: &str,
        body: &'a [Step],
        path: &StepPath,
    ) -> Result<(), Stop> {
        let secret = self.is_secret(items);
        let iterations = self.replayed_iterations(path);
        let shadowed = self.vars.get(item).cloned();
        let shadowed_secret = self.secret_vars.contains(item);
        for iteration in iterations {
            // A body the record has nothing for runs again from the items as
            // they are now; an empty body binds nothing.
            let elements = if body.is_empty() {
                Vec::new()
            } else {
                match self.evaluate(items) {
                    Ok(Some(Value::Array(elements))) => elements,
                    Ok(_) | Err(_) => Vec::new(),
                }
            };
            if let Some(element) = elements.into_iter().nth(iteration) {
                if secret {
                    self.secret_vars.insert(item.to_owned());
                } else {
                    self.secret_vars.remove(item);
                }
                self.vars.insert(item.to_owned(), element);
            }
            self.block(body, path.each(Some(iteration))).await?;
        }
        if shadowed_secret {
            self.secret_vars.insert(item.to_owned());
        } else {
            self.secret_vars.remove(item);
        }
        match shadowed {
            Some(value) => self.vars.insert(item.to_owned(), value),
            None => self.vars.remove(item),
        };
        Ok(())
    }

    /// The iterations of the loop at `path` an earlier run recorded, oldest
    /// first.
    fn replayed_iterations(&self, path: &StepPath) -> Vec<usize> {
        // The record spells an iteration `1.each[0].0`: the loop's own path,
        // then the iteration in brackets, then a step inside its body.
        let body = path.each(None);
        let opened = format!("{}[", body.as_str());
        let mut seen = BTreeSet::new();
        self.resumed
            .iter()
            .filter_map(|record| {
                let rest = record.path.as_str().strip_prefix(opened.as_str())?;
                let (digits, rest) = rest.split_once(']')?;
                rest.starts_with('.').then_some(())?;
                digits.parse().ok()
            })
            // An iteration's steps are recorded one after another: only the
            // first of each names the iteration to walk.
            .filter(|iteration| seen.insert(*iteration))
            .collect()
    }

    /// Take the step at `path` from the earlier run that recorded it, and go
    /// on from its own outcome: a loop from the iterations it made, a branch
    /// from the one it took, and a step below either from what it produced.
    async fn replay_step(
        &mut self,
        step: &'a Step,
        path: &StepPath,
        done: Done,
    ) -> Result<(), Stop> {
        // A block is taken for what it decided, and what it decided is the
        // output its record shows.
        let decided = match &done.result {
            Ok(output) => output.clone(),
            Err(_) => None,
        };
        match &step.kind {
            StepKind::ForEach {
                items, item, body, ..
            } => {
                self.record(step, path, done, Recovered::Stopped)?;
                self.replayed(items, item, body, path).await
            }
            StepKind::If {
                then, otherwise, ..
            } => {
                let branch = match &decided {
                    Some(Value::Bool(holds)) => Some((
                        if *holds { then } else { otherwise },
                        if *holds {
                            path.then_branch()
                        } else {
                            path.else_branch()
                        },
                    )),
                    _ => None,
                };
                self.record(step, path, done, Recovered::Stopped)?;
                match branch {
                    Some((steps, at)) => self.block(steps, at).await,
                    // An answer of some other shape names no branch: nothing
                    // below it is taken as run.
                    None => Ok(()),
                }
            }
            _ => self.record(step, path, done, Recovered::Stopped),
        }
    }

    /// Run a step under its [`StepPolicy`]: once, more than once, or with a
    /// fallback or a declared default when it fails. The record says what
    /// happened however it went.
    async fn settled_step(&mut self, step: &'a Step, path: StepPath) -> Result<(), Stop> {
        let (sent, mut done) = self.step(step, &path).await?;
        if done.recorded {
            return Ok(());
        }
        done.attempts = sent;
        if done.result.is_ok() {
            return self.record(step, &path, done, Recovered::Stopped);
        }
        let (attempt, failed) = self.retry(step, &path, &mut done).await?;
        if failed.is_none() {
            return self.record(step, &path, done, Recovered::Retried);
        }
        let failure = failed.unwrap_or(StepFailure::Abandoned);
        if failure.ends_run() {
            self.push_record(step, &path, done, Recovered::Stopped);
            return Err((path, failure));
        }
        match &step.on_error {
            // The step's own effect, if it had one, stands: a fallback adds
            // to the run, it does not take back what happened.
            StepPolicy::Fallback { steps } => {
                self.record(step, &path, done, Recovered::Fallback)?;
                self.note_failed(step, failure, false, Recovered::Fallback);
                self.block(steps, path.child(0)).await
            }
            StepPolicy::Default { value } => match self.evaluate(value) {
                Ok(fallback) => {
                    if let Some(fallback) = fallback {
                        self.outputs.insert(step.id.clone(), fallback);
                    }
                    self.note_failed(step, failure, true, Recovered::Default);
                    self.record(step, &path, done, Recovered::Default)
                }
                Err(why) => {
                    self.record(step, &path, done, Recovered::Stopped)?;
                    Err((path, why))
                }
            },
            StepPolicy::Fail | StepPolicy::Retry { .. } => {
                debug_assert!(attempt > 0 || failure.code() == "abandoned");
                self.record(step, &path, done, Recovered::Stopped)
            }
        }
    }

    /// Try `done` again while its [`StepPolicy`] says to and another try
    /// could go differently, sending the same invocation so nothing is
    /// applied twice. Answers the number of tries made and, when the step
    /// is still failing, why.
    async fn retry(
        &mut self,
        step: &'a Step,
        path: &StepPath,
        done: &mut Done,
    ) -> Result<(u32, Option<StepFailure>), Stop> {
        let StepPolicy::Retry { attempts, .. } = &step.on_error else {
            return Ok((1, done.result.as_ref().err().cloned()));
        };
        let mut made = 1;
        while made < (*attempts).max(1) {
            let Some(failure) = done.result.as_ref().err() else {
                return Ok((made, None));
            };
            // An action may have moved on, or the person may have stopped
            // the run: another try would not go differently for either.
            if !done.retryable || !failure.transient() {
                break;
            }
            let policy = step.on_error.clone();
            let made_next = made.saturating_add(1);
            if !self.wait_before(&policy, made_next, path).await {
                break;
            }
            made = made_next;
            let (sent, again) = self.step(step, path).await?;
            done.attempts.extend(sent);
            done.result = again.result;
            done.invocation = again.invocation;
        }
        Ok((made, done.result.as_ref().err().cloned()))
    }

    /// Wait before trying a step again, unless the run was told to stop
    /// first. Returns whether it is still going.
    async fn wait_before(&self, policy: &StepPolicy, attempt: u32, path: &StepPath) -> bool {
        let wait = policy.wait_before(attempt);
        if wait.is_zero() {
            return self.stopped(path).await.is_ok();
        }
        // Nobody moves this wait: only the clock ends it, and a stop ends it
        // sooner. The timer belongs to whoever awaits, so the interpreter
        // needs none of its own.
        let stopped = self.hooks.stopped(StopWatch {
            path,
            since: std::time::Instant::now(),
        });
        matches!(
            wait_for(
                std::future::pending::<Result<(), StepFailure>>(),
                stopped,
                wait,
                false,
            )
            .await,
            Wait::Moved(Ok(())) | Wait::TimedOut
        )
    }

    /// Remember a step that did not succeed, so a later step reading it can
    /// say so instead of reading nothing.
    fn note_failed(
        &mut self,
        step: &Step,
        failure: StepFailure,
        output_is_none: bool,
        recovered: Recovered,
    ) {
        self.failed.insert(
            step.id.clone(),
            FailedStep {
                failure,
                output_is_none: output_is_none && produces_no_resource(&step.kind),
                recovered,
            },
        );
    }

    /// Append the record of a finished step, saying what the run did about
    /// it when it failed, and stop the run when `recovered` says it did not
    /// carry on.
    fn record(
        &mut self,
        step: &Step,
        path: &StepPath,
        done: Done,
        recovered: Recovered,
    ) -> Result<(), Stop> {
        match self.push_record(step, path, done, recovered) {
            Some(failure) if invokes(&step.kind) => Err((path.clone(), failure)),
            _ => Ok(()),
        }
    }

    /// Append the record of a finished block step and always carry on: an
    /// `if` and a loop have no effect of their own to have failed with.
    fn record_block(&mut self, step: &Step, path: &StepPath, done: Done) {
        self.push_record(step, path, done, Recovered::Stopped);
    }

    /// Append a step's record, with what the run did about it when it did
    /// not succeed, and answer the failure that ends the run here when the
    /// step was not carried on from.
    fn push_record(
        &mut self,
        step: &Step,
        path: &StepPath,
        done: Done,
        recovered: Recovered,
    ) -> Option<StepFailure> {
        // A run that was stopped ends as cancelled: it did not fail at the
        // step, it was told to stop there, and what it did until then
        // stands. The record says which step, so a resumed run can say where
        // it stopped.
        if matches!(done.result, Err(StepFailure::Stopped { .. })) {
            self.stopped = Some(path.clone());
        }
        let attempt = u32::try_from(done.attempts.len())
            .unwrap_or(u32::MAX)
            .max(1);
        let (status, degraded, halt) = match done.result {
            Ok(output) => {
                let shown = output.as_ref().map(|value| redacted(step, value));
                if let Some(value) = output {
                    self.outputs.insert(step.id.clone(), value);
                }
                (
                    StepStatus::Succeeded { output: shown },
                    StepOutcome::None,
                    None,
                )
            }
            Err(failure) => {
                let degraded = StepOutcome::degraded(failure.clone(), attempt, recovered);
                let window = StepStatus::Failed {
                    failure: failure.clone(),
                    attempt,
                    recovered,
                };
                let halt = recovered.is_stopped().then_some(failure);
                (window, degraded, halt)
            }
        };
        // A run that stopped at the step is a failed run, not a degraded
        // one: the step's record says what happened, and the run's outcome
        // says it ended there.
        if !degraded.is_none() && !recovered.is_stopped() {
            self.degraded.push(degraded.clone());
        }
        self.records.push(StepRecord {
            path: path.clone(),
            step: step.id.clone(),
            invocation: done.invocation,
            attempts: done.attempts,
            degraded,
            status,
        });
        halt
    }

    /// Run a step once, answering how it went and every invocation it sent.
    async fn step(
        &mut self,
        step: &'a Step,
        path: &StepPath,
    ) -> Result<(Vec<InvocationId>, Done), Stop> {
        let mut sent = Vec::new();
        let done = self.attempt(step, path, &mut sent).await?;
        Ok((sent, done))
    }

    async fn attempt(
        &mut self,
        step: &'a Step,
        path: &StepPath,
        sent: &mut Vec<InvocationId>,
    ) -> Result<Done, Stop> {
        // A step an earlier run already took is taken as it was: this run
        // goes on from what that step left instead of doing it again.
        //
        // That step's own block is walked here too (a loop's iterations, a
        // branch's steps), so `recorded` says the record is already written
        // and [`Runner::settled_step`] has nothing left to decide.
        if let Some(done) = self.continues(path, &step.id) {
            return self
                .replay_step(step, path, done)
                .await
                .map(|()| Done::recorded());
        }
        // A step that would invoke something is not begun once the run is
        // stopped: nothing it could do is left to take back. A question or an
        // assignment is only ever recorded as not taken.
        if invokes(&step.kind)
            && let Err(reason) = self.stopped(path).await
        {
            let stopped = StepFailure::Stopped { reason };
            return Ok(Done::local(Err(stopped)));
        }
        if let Some(failed) = self.failed_dependency(step) {
            return Ok(Done {
                invocation: None,
                attempts: Vec::new(),
                retryable: false,
                recorded: false,
                result: Err(failed),
            });
        }
        let mut done = self.kind(step, path, sent).await?;
        done.retryable = matches!(
            step.kind,
            StepKind::Invoke { .. } | StepKind::Ai { .. } | StepKind::Task { .. }
        );
        Ok(done)
    }

    /// The failure that a step reading a step which did not succeed reports
    /// in place of quietly reading nothing: the one with the same code as
    /// the failure it names, so a branch two steps down matches it as well.
    fn failed_dependency(&self, step: &'a Step) -> Option<StepFailure> {
        for name in reads(step) {
            let Some(failed) = self.failed.get(&name) else {
                continue;
            };
            // A step that was passed over with its default stands in for
            // itself: a later step reads that value, and the reason it is
            // there at all is the run's degraded note, not a second failure.
            let stands_in = failed.recovered == Recovered::Default && !failed.output_is_none;
            if stands_in && self.outputs.contains_key(&name) {
                continue;
            }
            return Some(failed.failure.clone());
        }
        None
    }

    /// Run one step's own work. A step with a body records itself through
    /// [`record_block`](Self::record_block) and answers an empty
    /// [`Done`](Done); its body's failure is the step's failure.
    async fn kind(
        &mut self,
        step: &'a Step,
        path: &StepPath,
        sent: &mut Vec<InvocationId>,
    ) -> Result<Done, Stop> {
        match &step.kind {
            StepKind::Invoke {
                action,
                resource,
                args,
                ..
            } => {
                let done = self.invoke(step, path, action, resource, args).await;
                if let Some(invocation) = done.invocation {
                    sent.push(invocation);
                }
                Ok(done)
            }
            StepKind::Ai {
                prompt,
                output,
                input,
            } => {
                // A secret handed to the agent would reach a model provider.
                if input.as_ref().is_some_and(|input| self.is_secret(input)) {
                    let refused = StepFailure::Binding {
                        reason: "an AI step may not read a secret".into(),
                    };
                    return Ok(Done::local(Err(refused)));
                }
                let transform = AiTransform::Freeform {
                    prompt: prompt.clone(),
                    output: output.clone(),
                };
                let done = self
                    .ai(step, path, &transform, input.as_ref(), output.as_ref())
                    .await;
                if let Some(invocation) = done.invocation {
                    sent.push(invocation);
                }
                Ok(done)
            }
            StepKind::Task {
                instruction,
                allow,
                output,
            } => {
                let transform = AiTransform::Task {
                    instruction: instruction.clone(),
                    allow: allow.clone(),
                    output: output.clone(),
                };
                let done = self.ai(step, path, &transform, None, output.as_ref()).await;
                if let Some(invocation) = done.invocation {
                    sent.push(invocation);
                }
                Ok(done)
            }
            StepKind::If {
                cond,
                then,
                otherwise,
            } => {
                let holds = self.condition(cond);
                let taken = holds.as_ref().ok().copied();
                let done = Done::local(holds.map(|b| Some(Value::Bool(b))));
                self.record_block(step, path, done);
                match taken {
                    Some(true) => self.block(then, path.then_branch()).await,
                    Some(false) => self.block(otherwise, path.else_branch()).await,
                    None => Ok(()),
                }?;
                Ok(Done::recorded())
            }
            StepKind::ForEach { items, item, body } => {
                self.for_each(step, path.clone(), items, item, body).await?;
                Ok(Done::recorded())
            }
            StepKind::SetVar { name, value } => {
                let result = match self.evaluate(value) {
                    Ok(Some(evaluated)) => {
                        if self.is_secret(value) {
                            self.secret_vars.insert(name.clone());
                        } else {
                            self.secret_vars.remove(name);
                        }
                        self.vars.insert(name.clone(), evaluated);
                        Ok(None)
                    }
                    Ok(None) => Err(StepFailure::Binding {
                        reason: format!("nothing to assign to `{name}`"),
                    }),
                    Err(failure) => Err(failure),
                };
                Ok(Done::local(result))
            }
            StepKind::Ask { param } => Ok(Done::local(self.ask(step, path, param).await)),
        }
    }

    /// A loop step: bind `item` to each element of `items` in turn and run
    /// `body`, restoring what `item` named (and whether it was secret)
    /// afterwards.
    async fn for_each(
        &mut self,
        step: &'a Step,
        path: StepPath,
        items: &Binding,
        item: &str,
        body: &'a [Step],
    ) -> Result<(), Stop> {
        let elements = match self.evaluate(items) {
            Ok(Some(Value::Array(elements))) => Ok(elements),
            Ok(_) => Err(StepFailure::Binding {
                reason: "a loop needs an array".into(),
            }),
            Err(failure) => Err(failure),
        };
        let count = elements.as_ref().map(|e| Some(Value::from(e.len())));
        let done = Done::local(count.map_err(Clone::clone));
        self.record_block(step, &path, done);
        let elements = elements.unwrap_or_default();
        let shadowed = self.vars.get(item).cloned();
        let shadowed_secret = self.secret_vars.contains(item);
        let secret = self.is_secret(items);
        for (iteration, element) in elements.into_iter().enumerate() {
            if secret {
                self.secret_vars.insert(item.to_owned());
            } else {
                self.secret_vars.remove(item);
            }
            self.vars.insert(item.to_owned(), element);
            self.block(body, path.each(Some(iteration))).await?;
        }
        if shadowed_secret {
            self.secret_vars.insert(item.to_owned());
        } else {
            self.secret_vars.remove(item);
        }
        match shadowed {
            Some(value) => self.vars.insert(item.to_owned(), value),
            None => self.vars.remove(item),
        };
        Ok(())
    }

    async fn ask(
        &self,
        step: &Step,
        path: &StepPath,
        param: &ActionParam,
    ) -> Result<Option<Value>, StepFailure> {
        if self.mode == RunMode::Unattended {
            return Err(StepFailure::NeedsPerson);
        }
        let answer = self
            .hooks
            .ask(AskRequest {
                path: path.clone(),
                step: step.id.clone(),
                param: param.clone(),
            })
            .await
            .flatten()
            .ok_or(StepFailure::Declined)?;
        let mut supplied = Map::new();
        supplied.insert(param.name.to_string(), answer.clone());
        let mut required = param.clone();
        required.required = true;
        ActionArgs::bind_json(std::slice::from_ref(&required), &supplied).map_err(|error| {
            StepFailure::Arguments {
                reason: error.to_string(),
            }
        })?;
        Ok(Some(answer))
    }

    /// One AI invocation of the agent's transforms: the step's declared
    /// output is what the agent is asked for, and what its answer is held to
    /// as well as the published result spec.
    async fn ai(
        &self,
        step: &Step,
        path: &StepPath,
        transform: &AiTransform,
        input: Option<&Binding>,
        output: Option<&Value>,
    ) -> Done {
        let mut done = self
            .invoke(
                step,
                path,
                &transform.key(),
                &Selector::FirstOfKind(AGENT_TRANSFORM_KIND.into()),
                &transform.arguments(input),
            )
            .await;
        done.result = done.result.and_then(|value| transformed(output, value));
        done
    }

    async fn invoke(
        &self,
        step: &Step,
        path: &StepPath,
        key: &ActionKey,
        selector: &Selector,
        args: &BTreeMap<String, Binding>,
    ) -> Done {
        let prepared = match self.prepare(step, path, key, selector, args).await {
            Ok(prepared) => prepared,
            Err(failure) => return Done::local(Err(failure)),
        };
        let invocation = prepared.request.invocation;
        let result = self
            .complete(prepared.request, path)
            .await
            .and_then(|output| {
                prepared
                    .result
                    .validate(&output)
                    .map_err(|reason| StepFailure::Contract { reason })?;
                Ok(result_value(&prepared.result, output))
            });
        Done {
            invocation: Some(invocation),
            attempts: Vec::new(),
            retryable: false,
            recorded: false,
            result,
        }
    }

    /// The appearance `binding` evaluates to, in the exact form a door
    /// admits: a value that breaks a rule fails the step rather than being
    /// cleaned up, so what readers see is what the program asked for.
    fn appearance(&self, binding: &Binding) -> Result<tilo_actions::Appearance, StepFailure> {
        let refused = |reason: String| StepFailure::Arguments { reason };
        let value = self
            .evaluate(binding)?
            .ok_or_else(|| refused("the appearance binding has no value".into()))?;
        let appearance: tilo_actions::Appearance = serde_json::from_value(value)
            .map_err(|error| refused(format!("the appearance is malformed: {error}")))?;
        appearance
            .validate()
            .map_err(|error| refused(format!("the appearance is refused: {error}")))?;
        Ok(appearance)
    }

    /// Everything up to the invocation itself: resolve the target, check the
    /// grant, bind the arguments and obtain any confirmation.
    async fn prepare(
        &self,
        step: &Step,
        path: &StepPath,
        key: &ActionKey,
        selector: &Selector,
        args: &BTreeMap<String, Binding>,
    ) -> Result<Prepared, StepFailure> {
        let (app_id, action) = (key.app.as_str(), key.action.as_str());
        let resource = match selector {
            Selector::Fixed(id) => Some(id.clone()),
            Selector::FirstOfKind(_) => None,
            Selector::FromBinding(binding) => match self.evaluate(binding)? {
                Some(Value::String(address)) => Some(address),
                _ => {
                    return Err(StepFailure::Binding {
                        reason: "the resource binding is not a resource address".into(),
                    });
                }
            },
        };
        if app_id == SHORTCUTS_APP_ID && action == SHORTCUT_RUN_ACTION {
            let called = resource.as_deref().and_then(shortcut_id_of);
            // A shortcut already running above this one is not run again: the
            // check that reads the saved library caught the chains it could
            // see, and this is what stops a loop between two shortcuts saved
            // before one of them learned to call the other. It also covers the
            // shortcut running itself.
            if called.is_some_and(|id| self.chain.iter().any(|running| *running == id)) {
                return Err(StepFailure::Recursion);
            }
        }
        let target = match (&resource, selector) {
            (Some(id), _) => ResourceTarget::Id(id),
            (None, Selector::FirstOfKind(kind)) => ResourceTarget::Kind(kind),
            (None, _) => ResourceTarget::Any,
        };
        let row = self.find(key, target).await?;
        let Some(descriptor) = row.action.as_ref() else {
            return Err(StepFailure::Unresolved {
                app_id: app_id.to_string(),
                action: action.to_string(),
                reason: "the row offers no action".into(),
            });
        };
        let mut supplied = Map::new();
        for (name, binding) in args {
            let Some(value) = self.evaluate(binding)? else {
                continue;
            };
            // A null bound to an optional parameter that takes no null means
            // "not given", as when a loop item has no parent to answer.
            let omitted = value.is_null()
                && descriptor
                    .params
                    .iter()
                    .any(|p| p.name.as_ref() == name.as_str() && !p.required && !p.nullable);
            if !omitted {
                supplied.insert(name.clone(), value);
            }
        }
        let granted = self.granted(app_id, action, descriptor, &supplied)?;
        if let tilo_actions::contract::Availability::Unavailable { code, reason } =
            &descriptor.contract.availability
        {
            return Err(StepFailure::Unavailable {
                code: code.clone(),
                reason: reason.clone(),
            });
        }
        let bound = ActionArgs::bind_json(&descriptor.params, &supplied).map_err(|error| {
            StepFailure::Arguments {
                reason: error.to_string(),
            }
        })?;
        let appearance = match &step.kind {
            StepKind::Invoke {
                appearance: Some(binding),
                ..
            } => Some(self.appearance(binding)?),
            _ => None,
        };
        let confirmation = self
            .confirmation(step, path, app_id, descriptor, &bound, &granted)
            .await?;
        let context = self.context(app_id, &row, descriptor, path);
        Ok(Prepared {
            request: InvokeRequest {
                on_behalf_of: None,
                invocation: invocation_id(self.run, path),
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: row.provider.clone(),
                    address: ActionAddress::new(
                        descriptor.address.resource.clone(),
                        descriptor.address.action.clone(),
                    ),
                },
                argument: if bound.is_empty() {
                    InvokeArgument::None
                } else {
                    InvokeArgument::Json(bound.to_json())
                },
                context: Some(context),
                confirmation,
                // Left unset so a retry re-sends an identical request even
                // after the catalogue moved on; the host re-resolves the
                // target itself.
                revision: None,
                wait_ms: None,
                appearance,
            },
            result: descriptor.contract.result.clone(),
        })
    }

    /// What the shortcut's grant answers for the step: a refusal, or an
    /// answer the step may proceed under.
    /// A field edit is decided where its path (in `supplied`) acts, as
    /// the room decides it.
    fn granted(
        &self,
        app_id: &str,
        action: &str,
        descriptor: &ActionDescriptor,
        supplied: &Map<String, Value>,
    ) -> Result<StepGrant, StepFailure> {
        let at = (action == FIELD_EDIT)
            .then(|| {
                supplied
                    .get(EDIT_PARAM)
                    .and_then(|edit| edit_reach(&Binding::Literal(edit.clone())))
            })
            .flatten();
        let node = match &at {
            Some(at) => step_node_at(
                self.caller.room(),
                app_id,
                &descriptor.resource_kind,
                &descriptor.address,
                at,
            ),
            None => step_node(
                self.caller.room(),
                app_id,
                &descriptor.resource_kind,
                &descriptor.address,
            ),
        };
        let granted = grant_answer(&self.shortcut.grant, &node, descriptor.contract.need().kind);
        let (app_id, action) = (app_id.to_string(), action.to_string());
        match (granted.answer, self.mode) {
            (Answer::Deny, _) => Err(StepFailure::OutsideGrant { app_id, action }),
            (Answer::Ask, RunMode::Unattended) => Err(StepFailure::AsksFirst { app_id, action }),
            _ => Ok(granted),
        }
    }

    async fn confirmation(
        &self,
        step: &Step,
        path: &StepPath,
        app_id: &str,
        descriptor: &ActionDescriptor,
        bound: &ActionArgs,
        granted: &StepGrant,
    ) -> Result<Option<Confirmation>, StepFailure> {
        let argument = format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::Value::Object(bound.to_json())
        );
        let policy = match descriptor
            .contract
            .confirmation_for_argument(Some(&argument))
        {
            ConfirmationPolicy::None
                if granted.answer == Answer::Ask || descriptor.contract.is_attended() =>
            {
                ConfirmationPolicy::Confirm
            }
            policy => policy,
        };
        if policy == ConfirmationPolicy::None {
            return Ok(None);
        }
        if self.mode == RunMode::Unattended {
            // Only a rule that names this exact action consents in advance
            // for a run nobody is present at, and nothing does for one that
            // needs its person attending.
            if !granted.names_action || descriptor.contract.is_attended() {
                return Err(StepFailure::ConfirmationRequired);
            }
            return Ok(Some(match policy {
                ConfirmationPolicy::Phrase { phrase } => Confirmation::Phrase(phrase),
                ConfirmationPolicy::Confirm | ConfirmationPolicy::None => Confirmation::Confirmed,
            }));
        }
        let mut arguments = bound.to_json();
        for param in &descriptor.params {
            if param.ty == ParamType::Secret
                && let Some(value) = arguments.get_mut(param.name.as_ref())
            {
                *value = Value::from(REDACTED);
            }
        }
        let answer = self
            .hooks
            .confirm(ConfirmationRequest {
                path: path.clone(),
                step: step.id.clone(),
                app_id: app_id.to_string(),
                descriptor: descriptor.clone(),
                policy,
                arguments,
            })
            .await
            .flatten()
            .ok_or(StepFailure::Declined)?;
        Ok(Some(answer))
    }

    fn context(
        &self,
        app_id: &str,
        row: &CatalogueRow,
        descriptor: &ActionDescriptor,
        path: &StepPath,
    ) -> ActionContext {
        let context = ActionContext::for_resource(
            row.resource.id.clone(),
            row.resource.kind.clone(),
            row.resource.label.clone(),
        )
        .with_app(app_id, app_id)
        .with_purpose(self.shortcut.name.clone());
        let context = match &self.parent {
            Some(parent) => context.with_group(
                parent.id.clone(),
                self.shortcut.name.clone(),
                parent.kind.clone(),
            ),
            None => context.with_group(
                self.run.0.to_string(),
                self.shortcut.name.clone(),
                "shortcut",
            ),
        };
        context
            .with_facet(ActionFacet::new("kind", "shortcut"))
            .with_facet(ActionFacet::new("trigger", self.trigger.clone()))
            .with_facet(ActionFacet::new("shortcut", self.shortcut.id.clone()))
            // With the run's group, names the step a change was: a host that
            // restarted mid-run finds the steps already taken in history.
            .with_facet(ActionFacet::new(STEP_FACET, path.as_str()))
            .with_caused_by(self.caused_by.clone())
            .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new(
                    app_id,
                    row.resource.kind.clone(),
                    descriptor.address.action.clone(),
                ),
                [],
            )))
    }

    /// The row offering `action` on `app_id` at `target`, fetched now so the
    /// run acts on what the person is offered at this moment.
    async fn find(
        &self,
        key: &ActionKey,
        target: ResourceTarget<'_>,
    ) -> Result<CatalogueRow, StepFailure> {
        let unresolved = |reason: String| StepFailure::Unresolved {
            app_id: key.app.clone(),
            action: key.action.clone(),
            reason,
        };
        let mut query = CatalogueQuery {
            target: match target {
                ResourceTarget::Id(id) => Some(ActionAddress::new(id, key.action.as_str())),
                ResourceTarget::Kind(_) | ResourceTarget::Any => None,
            },
            max_results: MAX_CATALOGUE_PAGE,
            ..CatalogueQuery::default()
        };
        for _ in 0..MAX_CATALOGUE_PAGES {
            let page = self
                .host
                .query(&self.caller, query.clone())
                .await
                .ok_or(StepFailure::Abandoned)?
                .map_err(|error| StepFailure::Host {
                    error: Box::new(error),
                })?;
            if let Some(row) = find_row(&page.rows, key, target) {
                return Ok(row.clone());
            }
            if page.exhausted || page.next_cursor.is_empty() {
                break;
            }
            query.cursor = page.next_cursor;
            query.revision = Some(page.revision);
        }
        Err(unresolved(match target {
            ResourceTarget::Id(id) => format!("`{id}` is not offered"),
            ResourceTarget::Kind(kind) => format!("no `{kind}` resource is offered"),
            ResourceTarget::Any => "nothing offers it".into(),
        }))
    }

    /// Invoke — or, when this run already did, pick up the earlier
    /// invocation — and wait for its outcome, or give up waiting when the
    /// caller stops the run or the step outstays its patience. Giving up
    /// never cancels the action itself: an effect that still lands is the
    /// host's to record like any other.
    async fn complete(
        &self,
        request: InvokeRequest,
        path: &StepPath,
    ) -> Result<ActionOutput, StepFailure> {
        let deadline = std::time::Instant::now().checked_add(self.timeout);
        let id = request.invocation;
        let mut status = match self.host.observe(&self.caller, &id) {
            Some(state) => state.receipt().status,
            None => {
                self.host
                    .invoke(&self.caller, request)
                    .await
                    .ok_or(StepFailure::Abandoned)?
                    .map_err(|error| StepFailure::Host {
                        error: Box::new(error),
                    })?
                    .status
            }
        };
        loop {
            status = match status {
                ReceiptStatus::Completed { output } => return Ok(output),
                ReceiptStatus::Failed { error } => {
                    return Err(StepFailure::Action {
                        error: Box::new(error),
                    });
                }
                ReceiptStatus::Rejected { error } => {
                    return Err(StepFailure::Host {
                        error: Box::new(error),
                    });
                }
                ReceiptStatus::Cancelled { message } => {
                    return Err(StepFailure::Cancelled { message });
                }
                ReceiptStatus::Accepted => {
                    let left = deadline.and_then(|deadline| {
                        deadline.checked_duration_since(std::time::Instant::now())
                    });
                    let Some(left) = left else {
                        return Err(self.timed_out());
                    };
                    // Wake when the host says the invocation may have moved;
                    // only a host that cannot tell falls back to the hooks'
                    // poll interval. A stop or a timeout ends the wait; the
                    // invocation itself is left to the host.
                    let change = self.host.next_change(&self.caller, &id);
                    let refreshing = change.is_none();
                    let moved = async {
                        match change {
                            Some(change) => change.await.ok_or(StepFailure::Abandoned),
                            None => self.hooks.settle(id).await.ok_or(StepFailure::Abandoned),
                        }
                    };
                    let stopped = self.hooks.stopped(StopWatch {
                        path,
                        since: std::time::Instant::now(),
                    });
                    match wait_for(moved, stopped, left, refreshing).await {
                        Wait::Moved(Ok(())) => {}
                        Wait::Moved(Err(failure)) => return Err(failure),
                        Wait::Stopped(reason) => return Err(StepFailure::Stopped { reason }),
                        Wait::TimedOut => return Err(self.timed_out()),
                    }
                    self.host
                        .observe(&self.caller, &id)
                        .ok_or_else(|| StepFailure::Host {
                            error: Box::new(ActionHostError::UnknownInvocation),
                        })?
                        .receipt()
                        .status
                }
            };
        }
    }

    fn timed_out(&self) -> StepFailure {
        StepFailure::TimedOut {
            seconds: self.timeout.as_secs(),
        }
    }

    /// Ask the caller whether the run must stop, naming the step it is at.
    /// A hook that gives no answer never stops a run.
    async fn stopped(&self, path: &StepPath) -> Result<(), String> {
        self.hooks
            .stopped(StopWatch {
                path,
                since: std::time::Instant::now(),
            })
            .await
            .unwrap_or(Ok(()))
    }

    fn condition(&self, condition: &Condition) -> Result<bool, StepFailure> {
        Ok(match condition {
            Condition::Truthy(value) => self.evaluate(value)?.as_ref().is_some_and(truthy),
            Condition::Equals { left, right } => self.evaluate(left)? == self.evaluate(right)?,
            Condition::Not(inner) => !self.condition(inner)?,
            Condition::All(all) => {
                for inner in all {
                    if !self.condition(inner)? {
                        return Ok(false);
                    }
                }
                true
            }
            Condition::Any(any) => {
                for inner in any {
                    if self.condition(inner)? {
                        return Ok(true);
                    }
                }
                false
            }
        })
    }

    /// The value of `binding` now; `None` for an optional input the run was
    /// not given, which leaves the argument it feeds unset.
    fn evaluate(&self, binding: &Binding) -> Result<Option<Value>, StepFailure> {
        let missing = |reason: String| StepFailure::Binding { reason };
        match binding {
            Binding::Literal(value) => Ok(Some(value.clone())),
            Binding::Input(name) => {
                let (name, pointer) = Binding::named_part(name);
                if !self
                    .shortcut
                    .inputs
                    .iter()
                    .any(|input| input.name.as_ref() == name)
                {
                    return Err(missing(format!("there is no input `{name}`")));
                }
                match self.inputs.get(name) {
                    None => Ok(None),
                    Some(value) => value.pointer(&pointer).cloned().map(Some).ok_or_else(|| {
                        missing(format!("input `{name}` has no value at `{pointer}`"))
                    }),
                }
            }
            Binding::Var(name) => {
                let (name, pointer) = Binding::named_part(name);
                self.vars
                    .get(name)
                    .ok_or_else(|| missing(format!("variable `{name}` is not set")))?
                    .pointer(&pointer)
                    .cloned()
                    .map(Some)
                    .ok_or_else(|| {
                        missing(format!("variable `{name}` has no value at `{pointer}`"))
                    })
            }
            Binding::StepOutput { step, pointer } => {
                let produced = match self.outputs.get(step) {
                    Some(value) => value.clone(),
                    // A step this run took from an earlier one is read from
                    // that run's invocation: the value that ran is the one a
                    // later step is given, never a second run of it.
                    None => match self.replayed_value(step) {
                        Some(value) => value,
                        None => {
                            return Err(missing(format!("step `{step}` produced no value")));
                        }
                    },
                };
                produced
                    .pointer(pointer)
                    .cloned()
                    .map(Some)
                    .ok_or_else(|| missing(format!("step `{step}` has no value at `{pointer}`")))
            }
            Binding::Template(text) => {
                let mut rendered = String::new();
                for piece in template::parse(text).map_err(missing)? {
                    match piece {
                        Piece::Text(text) => rendered.push_str(text),
                        Piece::Value(inner) => {
                            if self.is_secret(&inner) {
                                return Err(missing("a template may not include a secret".into()));
                            }
                            let value = self.evaluate(&inner)?.ok_or_else(|| {
                                missing("a template placeholder has no value".into())
                            })?;
                            rendered.push_str(&render_value(&value));
                        }
                    }
                }
                Ok(Some(Value::String(rendered)))
            }
            Binding::Object(members) => {
                let mut object = Map::new();
                for (name, member) in members {
                    if let Some(value) = self.evaluate(member)? {
                        object.insert(name.clone(), value);
                    }
                }
                Ok(Some(Value::Object(object)))
            }
            Binding::List(items) => items
                .iter()
                .map(|item| Ok(self.evaluate(item)?.unwrap_or(Value::Null)))
                .collect::<Result<Vec<_>, _>>()
                .map(|items| Some(Value::Array(items))),
        }
    }

    /// What step `step` produced, looked up on the host at the invocation the
    /// earlier run recorded it under.
    fn replayed_value(&self, step: &str) -> Option<Value> {
        let record = self
            .records
            .iter()
            .rev()
            .find(|record| record.step == step && record.invocation.is_some())?;
        let invocation = record.invocation?;
        let status = self
            .host
            .observe(&self.caller, &invocation)?
            .receipt()
            .status;
        match status {
            ReceiptStatus::Completed { output } => output.value,
            _ => None,
        }
    }

    fn is_secret(&self, binding: &Binding) -> bool {
        match binding {
            Binding::Input(name) => {
                let (name, _) = Binding::named_part(name);
                self.shortcut
                    .inputs
                    .iter()
                    .any(|input| input.name.as_ref() == name && input.ty == ParamType::Secret)
            }
            Binding::StepOutput { step, .. } => self.records.iter().any(|record| {
                record.step == *step
                    && matches!(
                        record.status,
                        StepStatus::Succeeded { output: Some(Value::String(ref shown)) }
                            if shown == REDACTED
                    )
            }),
            Binding::Var(name) => self.secret_vars.contains(Binding::named_part(name).0),
            Binding::Object(members) => members.values().any(|member| self.is_secret(member)),
            Binding::List(items) => items.iter().any(|item| self.is_secret(item)),
            Binding::Literal(_) | Binding::Template(_) => false,
        }
    }
}

/// How a step's output appears in history: an answer to a secret question
/// never does.
fn redacted(step: &Step, value: &Value) -> Value {
    match &step.kind {
        StepKind::Ask { param } if param.ty == ParamType::Secret => Value::from(REDACTED),
        _ => value.clone(),
    }
}

struct Prepared {
    request: InvokeRequest,
    result: ResultSpec,
}

/// The value a later step refers to: the JSON result, or the address of the
/// resource the action returned or, for a verb, of the item it created.
fn result_value(result: &ResultSpec, output: ActionOutput) -> Option<Value> {
    match result {
        ResultSpec::Json { .. } => output.value,
        ResultSpec::Resource { .. } | ResultSpec::None => {
            output.resources.into_iter().next().map(Value::String)
        }
        ResultSpec::Job => None,
    }
}

/// Hold an AI step's answer to the type it declared: the schema, or text
/// when it declared none. A value of the wrong shape is a broken contract,
/// never a value a later step has to guess about.
fn transformed(output: Option<&Value>, value: Option<Value>) -> Result<Option<Value>, StepFailure> {
    let contract = |reason: String| StepFailure::Contract { reason };
    let value = value.ok_or_else(|| contract("the agent returned nothing".into()))?;
    match output {
        // A declared shape is the step's own promise, so it holds the answer
        // whatever the published result spec allowed.
        Some(schema) => {
            let declared = serde_json::json!({
                "type": "object",
                "properties": {"result": schema}
            });
            let mut wrapper = Map::new();
            wrapper.insert("result".into(), value.clone());
            validate_json_schema(&Value::Object(wrapper), &declared).map_err(contract)?;
        }
        None if !value.is_string() => return Err(contract("the agent did not return text".into())),
        None => {}
    }
    Ok(Some(value))
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}
