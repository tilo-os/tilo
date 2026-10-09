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

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
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
use crate::grant::grant_permits;
use crate::path::StepPath;
use crate::shortcut::{
    AiTransform, Binding, Condition, Selector, Shortcut, Step, StepKind, UnattendedConsent,
};
use crate::template::{self, Piece, render_value};
use crate::{
    AGENT_APP_ID, AGENT_TRANSFORM_KIND, SHORTCUT_RUN_ACTION, SHORTCUTS_APP_ID, TriggerSpec,
    shortcut_id_of,
};

/// How many catalogue pages the interpreter reads looking for one action.
const MAX_CATALOGUE_PAGES: usize = 32;

/// What a secret looks like in a run report or a confirmation prompt.
const REDACTED: &str = "[redacted]";

/// Identity of one run. Reusing it reuses every invocation id the run
/// derives, which is what makes re-running it safe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// The invocation id of the step at `path` in run `run`.
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunTrigger {
    /// A person ran it.
    Manual,
    /// One of the shortcut's triggers fired.
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
    /// Confirmations given in advance that an unattended run may use in
    /// place of a person: only a step resolving to exactly one of these
    /// actions proceeds past its confirmation. The host passes the
    /// shortcut's [`consents`](Shortcut::consents) only to a run their
    /// giver started.
    pub consents: Vec<UnattendedConsent>,
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
    /// [`ActionHost::next_change`](tilo_actions::remote::ActionHost::next_change)
    /// cannot tell when the invocation changes. Abandoning the answer stops
    /// the run.
    fn settle(&self, invocation: InvocationId) -> Pending<()>;
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
    Failed {
        path: StepPath,
        failure: StepFailure,
    },
}

/// The history entry of one run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub run: RunId,
    pub shortcut: String,
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
        consents,
    } = request;
    let mut report = RunReport {
        run,
        shortcut: shortcut.id.clone(),
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
        consents,
        vars: BTreeMap::new(),
        outputs: BTreeMap::new(),
        records: Vec::new(),
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
        report.outcome = RunOutcome::Failed { path, failure };
    }
    report.steps = runner.records;
    report
}

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
    consents: Vec<UnattendedConsent>,
    vars: BTreeMap<String, Value>,
    outputs: BTreeMap<String, Value>,
    records: Vec<StepRecord>,
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
        match &step.kind {
            StepKind::Invoke {
                app_id,
                resource,
                action,
                args,
            } => {
                let done = self
                    .invoke(step, &path, app_id, resource, action, args)
                    .await;
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
                        AGENT_APP_ID,
                        &Selector::FirstOfKind(AGENT_TRANSFORM_KIND.into()),
                        transform.action(),
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
                for (iteration, element) in elements.into_iter().enumerate() {
                    self.vars.insert(item.clone(), element);
                    self.block(body, path.each(Some(iteration))).await?;
                }
                match shadowed {
                    Some(value) => self.vars.insert(item.clone(), value),
                    None => self.vars.remove(item),
                };
                Ok(())
            }
            StepKind::SetVar { name, value } => {
                let result = match self.evaluate(value) {
                    Ok(Some(value)) => {
                        self.vars.insert(name.clone(), value);
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
        app_id: &str,
        selector: &Selector,
        action: &str,
        args: &BTreeMap<String, Binding>,
    ) -> Done {
        let prepared = match self
            .prepare(step, path, app_id, selector, action, args)
            .await
        {
            Ok(prepared) => prepared,
            Err(failure) => return Done::local(Err(failure)),
        };
        let invocation = prepared.request.invocation;
        let result = self.complete(prepared.request).await.and_then(|output| {
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

    /// Everything up to the invocation itself: resolve the target, check the
    /// grant, bind the arguments and obtain any confirmation.
    async fn prepare(
        &self,
        step: &Step,
        path: &StepPath,
        app_id: &str,
        selector: &Selector,
        action: &str,
        args: &BTreeMap<String, Binding>,
    ) -> Result<Prepared, StepFailure> {
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
        let row = self.find(app_id, target, action).await?;
        let Some(descriptor) = row.action.as_ref() else {
            return Err(StepFailure::Unresolved {
                app_id: app_id.to_string(),
                action: action.to_string(),
                reason: "the row offers no action".into(),
            });
        };
        if !grant_permits(
            &self.shortcut.grant,
            app_id,
            &descriptor.address,
            &descriptor.resource_kind,
            descriptor.contract.permission,
        ) {
            return Err(StepFailure::OutsideGrant {
                app_id: app_id.to_string(),
                action: action.to_string(),
            });
        }
        if let tilo_actions::contract::Availability::Unavailable { code, reason } =
            &descriptor.contract.availability
        {
            return Err(StepFailure::Unavailable {
                code: code.clone(),
                reason: reason.clone(),
            });
        }
        let mut supplied = Map::new();
        for (name, binding) in args {
            if let Some(value) = self.evaluate(binding)? {
                supplied.insert(name.clone(), value);
            }
        }
        let bound = ActionArgs::bind_json(&descriptor.params, &supplied).map_err(|error| {
            StepFailure::Arguments {
                reason: error.to_string(),
            }
        })?;
        let confirmation = self
            .confirmation(step, path, app_id, descriptor, &bound)
            .await?;
        let context = self.context(app_id, &row, descriptor);
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
                fresh_binding: None,
                wait_ms: None,
            },
            result: descriptor.contract.result.clone(),
        })
    }

    async fn confirmation(
        &self,
        step: &Step,
        path: &StepPath,
        app_id: &str,
        descriptor: &ActionDescriptor,
        bound: &ActionArgs,
    ) -> Result<Option<Confirmation>, StepFailure> {
        let policy = descriptor.contract.advertised_confirmation();
        if policy == ConfirmationPolicy::None {
            return Ok(None);
        }
        if self.mode == RunMode::Unattended {
            if !self
                .consents
                .iter()
                .any(|consent| consent.covers(app_id, &descriptor.address))
            {
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
    ) -> ActionContext {
        ActionContext::for_resource(
            descriptor.action_label.clone(),
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
    }

    /// The row offering `action` on `app_id` at `target`, fetched now so the
    /// run acts on what the person is offered at this moment.
    async fn find(
        &self,
        app_id: &str,
        target: ResourceTarget<'_>,
        action: &str,
    ) -> Result<CatalogueRow, StepFailure> {
        let unresolved = |reason: String| StepFailure::Unresolved {
            app_id: app_id.to_string(),
            action: action.to_string(),
            reason,
        };
        let mut query = CatalogueQuery {
            target: match target {
                ResourceTarget::Id(id) => Some(ActionAddress::new(id, action)),
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
            if let Some(row) = find_row(&page.rows, app_id, target, action) {
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
    /// invocation — and wait for its outcome.
    async fn complete(&self, request: InvokeRequest) -> Result<ActionOutput, StepFailure> {
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
                    // Wake when the host says the invocation may have moved;
                    // only a host that cannot tell falls back to the hooks'
                    // poll interval.
                    match self.host.next_change(&self.caller, &id) {
                        Some(change) => change.await.ok_or(StepFailure::Abandoned)?,
                        None => self.hooks.settle(id).await.ok_or(StepFailure::Abandoned)?,
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
                if !self
                    .shortcut
                    .inputs
                    .iter()
                    .any(|input| input.name.as_ref() == name.as_str())
                {
                    return Err(missing(format!("there is no input `{name}`")));
                }
                Ok(self.inputs.get(name).cloned())
            }
            Binding::Var(name) => self
                .vars
                .get(name)
                .cloned()
                .map(Some)
                .ok_or_else(|| missing(format!("variable `{name}` is not set"))),
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
        }
    }

    fn is_secret(&self, binding: &Binding) -> bool {
        match binding {
            Binding::Input(name) => {
                self.shortcut.inputs.iter().any(|input| {
                    input.name.as_ref() == name.as_str() && input.ty == ParamType::Secret
                })
            }
            Binding::StepOutput { step, .. } => self.records.iter().any(|record| {
                record.step == *step
                    && matches!(
                        record.status,
                        StepStatus::Succeeded { output: Some(Value::String(ref shown)) }
                            if shown == REDACTED
                    )
            }),
            Binding::Literal(_) | Binding::Var(_) | Binding::Template(_) => false,
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
/// resource the action returned.
fn result_value(result: &ResultSpec, output: ActionOutput) -> Option<Value> {
    match result {
        ResultSpec::Json { .. } => output.value,
        ResultSpec::Resource { .. } => output.resources.into_iter().next().map(Value::String),
        ResultSpec::None | ResultSpec::Job => None,
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
