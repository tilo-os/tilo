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

/// Why a step did not complete. Every variant stops the run.
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum StepStatus {
    Succeeded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
    },
    Failed {
        failure: StepFailure,
    },
}

/// One step as it ran, in order. A loop body's steps appear once per
/// iteration, each with its own path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub path: StepPath,
    pub step: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<InvocationId>,
    #[serde(flatten)]
    pub status: StepStatus,
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
    #[serde(flatten)]
    pub outcome: RunOutcome,
}

impl RunReport {
    /// Every invocation id the run used, in order.
    #[must_use]
    pub fn invocations(&self) -> Vec<InvocationId> {
        self.steps
            .iter()
            .filter_map(|step| step.invocation)
            .collect()
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
    } = request;
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
        caused_by,
        timeout: shortcut
            .step_timeout_secs
            .map_or(DEFAULT_STEP_TIMEOUT, |secs| {
                std::time::Duration::from_secs(secs.max(1))
            }),
        stopped: None,
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
    caused_by: Option<tilo_actions::context::Cause>,
    /// How long each step of the run may take.
    timeout: std::time::Duration,
    /// The step the run was told to stop at, when it was.
    stopped: Option<StepPath>,
}

/// A step's result before it is recorded.
struct Done {
    invocation: Option<InvocationId>,
    result: Result<Option<Value>, StepFailure>,
}

impl Done {
    fn local(result: Result<Option<Value>, StepFailure>) -> Self {
        Self {
            invocation: None,
            result,
        }
    }
}

/// How often a wait looks again while its other answers are still pending.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Whether running `kind` invokes a published action — the steps a stop must
/// keep from starting, whose effect would otherwise land.
fn invokes(kind: &StepKind) -> bool {
    matches!(kind, StepKind::Invoke { .. } | StepKind::Ai { .. })
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
                self.step(step, path.child(index)).await?;
            }
            Ok(())
        })
    }

    /// Append the record of a finished step; a failure stops the run.
    fn record(&mut self, step: &Step, path: &StepPath, done: Done) -> Result<(), Stop> {
        // A run that was stopped ends as cancelled: it did not fail at the
        // step, it was told to stop there, and what it did until then
        // stands. The record says which step, so a resumed run can say where
        // it stopped.
        if matches!(done.result, Err(StepFailure::Stopped { .. })) {
            self.stopped = Some(path.clone());
        }
        let (status, halt) = match done.result {
            Ok(output) => {
                let shown = output.as_ref().map(|value| redacted(step, value));
                if let Some(value) = output {
                    self.outputs.insert(step.id.clone(), value);
                }
                (StepStatus::Succeeded { output: shown }, None)
            }
            Err(failure) => (
                StepStatus::Failed {
                    failure: failure.clone(),
                },
                Some((path.clone(), failure)),
            ),
        };
        self.records.push(StepRecord {
            path: path.clone(),
            step: step.id.clone(),
            invocation: done.invocation,
            status,
        });
        halt.map_or(Ok(()), Err)
    }

    async fn step(&mut self, step: &'a Step, path: StepPath) -> Result<(), Stop> {
        // A step that would invoke something is not begun once the run is
        // stopped: nothing it could do is left to take back. A question or an
        // assignment is only ever recorded as not taken.
        if invokes(&step.kind)
            && let Err(reason) = self.stopped(&path).await
        {
            let stopped = StepFailure::Stopped { reason };
            return self.record(step, &path, Done::local(Err(stopped)));
        }
        match &step.kind {
            StepKind::Invoke {
                action,
                resource,
                args,
                ..
            } => {
                let done = self.invoke(step, &path, action, resource, args).await;
                self.record(step, &path, done)
            }
            StepKind::Ai { transform, input } => {
                // A secret handed to the agent would reach a model provider.
                if self.is_secret(input) {
                    let refused = StepFailure::Binding {
                        reason: "an AI step may not read a secret".into(),
                    };
                    return self.record(step, &path, Done::local(Err(refused)));
                }
                let mut done = self
                    .invoke(
                        step,
                        &path,
                        &transform.key(),
                        &Selector::FirstOfKind(AGENT_TRANSFORM_KIND.into()),
                        &transform.arguments(input),
                    )
                    .await;
                done.result = done.result.and_then(|value| transformed(transform, value));
                self.record(step, &path, done)
            }
            StepKind::If {
                cond,
                then,
                otherwise,
            } => {
                let holds = self.condition(cond);
                let taken = holds.as_ref().ok().copied();
                self.record(
                    step,
                    &path,
                    Done::local(holds.map(|b| Some(Value::Bool(b)))),
                )?;
                match taken {
                    Some(true) => self.block(then, path.then_branch()).await,
                    Some(false) => self.block(otherwise, path.else_branch()).await,
                    None => Ok(()),
                }
            }
            StepKind::ForEach { items, item, body } => {
                self.for_each(step, path, items, item, body).await
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
                self.record(step, &path, Done::local(result))
            }
            StepKind::Ask { param } => {
                let result = self.ask(step, &path, param).await;
                self.record(step, &path, Done::local(result))
            }
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
        self.record(step, &path, Done::local(count.map_err(Clone::clone)))?;
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
        if app_id == SHORTCUTS_APP_ID
            && action == SHORTCUT_RUN_ACTION
            && resource
                .as_deref()
                .and_then(shortcut_id_of)
                .is_none_or(|id| id == self.shortcut.id)
        {
            return Err(StepFailure::Recursion);
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
        let granted = grant_answer(&self.shortcut.grant, &node, descriptor.contract.kind);
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
        ActionContext::for_resource(
            row.resource.id.clone(),
            row.resource.kind.clone(),
            row.resource.label.clone(),
        )
        .with_app(app_id, app_id)
        .with_purpose(self.shortcut.name.clone())
        .with_group(
            self.run.0.to_string(),
            self.shortcut.name.clone(),
            "shortcut",
        )
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
            Binding::StepOutput { step, pointer } => self
                .outputs
                .get(step)
                .ok_or_else(|| missing(format!("step `{step}` produced no value")))?
                .pointer(pointer)
                .cloned()
                .map(Some)
                .ok_or_else(|| missing(format!("step `{step}` has no value at `{pointer}`"))),
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

/// Hold an agent transform's result to what the step asked for.
fn transformed(
    transform: &AiTransform,
    value: Option<Value>,
) -> Result<Option<Value>, StepFailure> {
    let contract = |reason: String| StepFailure::Contract { reason };
    let value = value.ok_or_else(|| contract("the agent returned nothing".into()))?;
    match transform {
        AiTransform::Extract { schema } => {
            validate_json_schema(&value, schema).map_err(contract)?;
        }
        AiTransform::Classify { choices } => {
            if !value
                .as_str()
                .is_some_and(|label| choices.iter().any(|choice| choice == label))
            {
                return Err(contract("the agent chose none of the labels".into()));
            }
        }
        AiTransform::Summarise | AiTransform::Freeform { .. } => {
            if !value.is_string() {
                return Err(contract("the agent did not return text".into()));
            }
        }
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
