//! The API a program's runs act through.
//!
//! Every step of a program's run is an invocation here, as the program: the caller
//! must be this installation's `Caller::bot`. The change names the program, its
//! installer, the invocation it came from and the run's cause. The replica
//! decides with the installer's access bounded by the program's grants, so a
//! program never does more than its installer can now.
//!
//! A step its run already took (the host restarted mid-run) is found in the
//! record's history by its derived invocation id and observed instead of
//! taken again. An outside step completes when the outbox entries it
//! queued are settled: the API hands each entry to the connector's worker
//! and settles it as the program with what came back, so the request, its
//! result and the settle are all the program's, and the step's value is the
//! result the next step reads. A refusal settles failed; an unavailable
//! service leaves the entry pending and the step unavailable, so the run
//! is retried in order and the entry performed again.
//!
//! A program that runs for a person ([`ProgramApi::runs_for`]) is also offered
//! the steps the host answers itself for that person
//! ([`super::host_steps`]): an instruction handed to their assistant, and a
//! judgement asked of their model. Any program whose host keeps things for
//! it between its runs ([`ProgramApi::keeps`]) is offered its counts and its
//! note.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value, json};
use tilo_actions::api::{ActionAddress, ApiError};
use tilo_actions::context::Cause;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, ResourceAddress, ResourceCopy, ResourceReads, ResourceWake,
};
use tilo_actions::{
    ActionContext, ActionFailure, ActionInvoker, ActionOutput, Actor, InvocationId,
    InvocationUpdate, MemberId,
};
use tilo_peer_state::document_frame::DocumentRoute;

use super::host_steps::{self, Asked, AskedFor, HostStepError, HostSteps, Keeps, StepCause};
use super::item_json::OutboxRow;
use tilo_agent_session::record_actions::{api_status, resource_of, resource_provider};
use tilo_lock::MutexRecover as _;

/// What the API needs of the program's session and replicas.
pub trait ApiMember: Send {
    fn workspace(&self) -> &str;
    /// The loaded records and what each offers the program now.
    /// Every operation of each loaded record, each in the row of the resource
    /// holding it: what a caller listing one resource of a record reads its
    /// children by.
    fn offered(&self) -> Vec<(DocumentRoute, Vec<CatalogueRow>)>;
    /// Whether the record at `route` is loaded.
    fn has_record(&self, route: &DocumentRoute) -> bool;
    /// The record at `route` as the program may read it now: its whole document,
    /// `None` for one the workspace holds none of, the refusal for one the
    /// program may not read.
    fn record(&self, route: &DocumentRoute) -> Result<Option<Vec<u8>>, String>;
    /// Invoke as `invoker` within `context` and commit the change.
    /// `confirmed`: the run confirmed it, from the grant's advance consent,
    /// for an action whose contract asks for confirmation.
    fn invoke(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        invoker: &ActionInvoker,
        context: &ActionContext,
        confirmed: bool,
    ) -> Result<ActionOutput, ApiError>;
    /// The entry `actor` recorded for step `step` of run `run` in any loaded
    /// record.
    fn find_in_history(
        &self,
        step: &AutomationStep,
        actor: &Actor,
    ) -> Option<(DocumentRoute, ActionContext)>;
    /// The outbox stored at `field` of the record at `route`, in order.
    fn outbox(&self, route: &DocumentRoute, field: &str) -> Vec<OutboxRow>;
    /// The ids of the entries the change `actor` recorded for step `step`
    /// put into the outbox at `field` of the record at `route`.
    fn queued_in_history(
        &self,
        route: &DocumentRoute,
        step: &AutomationStep,
        actor: &Actor,
        field: &str,
    ) -> Vec<String>;
}

/// The context of the latest change in `doc` that `actor` recorded for step
/// `step`, with the change.
pub(crate) fn step_change(
    doc: &automerge::Automerge,
    step: &AutomationStep,
    actor: &Actor,
) -> Option<(automerge::Change, ActionContext)> {
    doc.get_changes(&[]).into_iter().rev().find_map(|change| {
        let context = ActionContext::from_change_message(change.message()?)?;
        let by = context.invoker.as_ref().map(|invoker| &invoker.acting.by);
        (AutomationStep::of(&context).as_ref() == Some(step) && by == Some(actor))
            .then(|| (change.clone(), context))
    })
}

/// The ids of the entries the change `actor` recorded for step `step` in
/// `doc` put into the outbox at `field`: those there after it and not
/// before it.
pub(crate) fn queued_by_step(
    doc: &automerge::Automerge,
    step: &AutomationStep,
    actor: &Actor,
    field: &str,
) -> Vec<String> {
    let Some((change, _)) = step_change(doc, step, actor) else {
        return Vec::new();
    };
    let before: std::collections::BTreeSet<String> =
        super::item_json::outbox_entries(doc, change.deps(), field)
            .into_iter()
            .map(|row| row.id)
            .collect();
    super::item_json::outbox_entries(doc, &[change.hash()], field)
        .into_iter()
        .map(|row| row.id)
        .filter(|id| !before.contains(id))
        .collect()
}

/// Which step of which invocation a change was: the invocation's group and
/// the step facet every step's context carries. History keeps both, so one
/// retried after a restart finds the steps it already took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomationStep {
    /// The invocation's group id.
    pub invocation: String,
    /// The step's path.
    pub step: String,
}

impl AutomationStep {
    #[must_use]
    pub fn of(context: &ActionContext) -> Option<Self> {
        let group = context
            .group
            .as_ref()
            .filter(|group| group.kind == "shortcut")?;
        let step = context
            .facets
            .iter()
            .find(|facet| facet.key == tilo_automation::STEP_FACET)?;
        Some(Self {
            invocation: group.id.clone(),
            step: step.value.clone(),
        })
    }
}

/// Why a connector did not perform an op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PerformError {
    Refused(String),
    /// The service or worker is unavailable now; it may succeed later.
    Unavailable(String),
}

/// A connector's worker, performing one queued outbox entry.
pub trait Performer: Send + Sync {
    /// Perform `queued` (the entry's value) and answer the service's result.
    fn perform(&self, queued: Value) -> Result<Value, PerformError>;
}

/// The outbox of the records of one app, and the worker that drains it.
pub struct OutboxSpec {
    /// The app whose records queue the entries.
    pub app_id: String,
    pub field: String,
    /// The connector's worker.
    pub connector: Arc<dyn Performer>,
}

/// The `ActionProvider` one installation's runs act through.
pub struct ProgramApi<P: ApiMember> {
    member: Arc<Mutex<P>>,
    installation: String,
    installer: String,
    outboxes: Vec<OutboxSpec>,
    cause: Mutex<Option<Cause>>,
    /// Steps taken since the API was made, by invocation.
    log: Mutex<HashMap<InvocationId, ReceiptStatus>>,
    /// The person the program runs for (hex root) and what the host answers
    /// for them, for a program that runs for one.
    host: OnceLock<(String, Arc<dyn HostSteps>)>,
    /// What the program keeps between its runs, where its host keeps any.
    keeps: OnceLock<Arc<dyn Keeps>>,
}

impl<P: ApiMember> ProgramApi<P> {
    /// The API of installation `installation`, installed by `installer`.
    pub fn new(
        member: Arc<Mutex<P>>,
        installation: String,
        installer: String,
        outboxes: Vec<OutboxSpec>,
    ) -> Self {
        Self {
            member,
            installation,
            installer,
            outboxes,
            cause: Mutex::new(None),
            log: Mutex::new(HashMap::new()),
            host: OnceLock::new(),
            keeps: OnceLock::new(),
        }
    }

    /// Run the program for `person` (hex root): the steps that ask the host
    /// itself, an instruction to that person's assistant and a judgement of
    /// their model, are offered to it and answered through `steps`. A program runs for one person from its
    /// start, so a second call changes nothing.
    pub fn runs_for(&self, person: String, steps: Arc<dyn HostSteps>) {
        let _ = self.host.set((person, steps));
    }

    /// Keep the program's counts and its note in `kept` between its runs:
    /// the steps that count and that change the note are offered to it and
    /// answered there, and the note goes with every instruction and
    /// judgement the host is asked for. Set once, at the program's start.
    pub fn keeps(&self, kept: Arc<dyn Keeps>) {
        let _ = self.keeps.set(kept);
    }

    /// Forget the steps this api took. For a run whose changes the workspace
    /// refused part of: run again, it finds the steps whose changes stand
    /// in the records' history and takes the others anew, reading the workspace
    /// as it now is, instead of answering every step from what it did the
    /// first time.
    pub fn forget_steps(&self) {
        self.log.lock_recover().clear();
    }

    /// The cause every change of the next run records.
    pub fn set_cause(&self, cause: Option<Cause>) {
        *self.cause.lock_recover() = cause;
    }

    fn cause(&self) -> Option<Cause> {
        self.cause.lock_recover().clone()
    }

    fn actor(&self) -> Actor {
        Actor::Member(MemberId::bot(self.installation.clone()))
    }

    fn its_own(&self, caller: &Caller) -> bool {
        is_bot(caller, &self.installation)
    }

    fn invoker(&self) -> ActionInvoker {
        ActionInvoker::bot(self.installation.clone()).acting_for(self.installer.clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, P> {
        self.member.lock_recover()
    }

    fn catalogue(&self, query: &CatalogueQuery) -> CataloguePage {
        let member = self.lock();
        let workspace = member.workspace().to_owned();
        let mut rows = Vec::new();
        let mut providers = Vec::new();
        for (route, offered) in member.offered() {
            let provider = resource_provider(&route);
            if query
                .provider
                .as_ref()
                .is_some_and(|wanted| *wanted != provider)
            {
                continue;
            }
            providers.push(CatalogueProvider {
                provider: provider.clone(),
                app_id: route.app_id.clone(),
                workspace: workspace.clone(),
                document_id: route.document.clone(),
                browsable: false,
            });
            rows.extend(offered.into_iter().map(|mut row| {
                row.provider.clone_from(&provider);
                row.app_id.clone_from(&route.app_id);
                row
            }));
        }
        drop(member);
        if query
            .provider
            .as_deref()
            .is_none_or(|wanted| wanted == host_steps::PROVIDER)
        {
            if self.host.get().is_some() {
                rows.extend(host_steps::rows());
            }
            if self.keeps.get().is_some() {
                rows.extend(host_steps::kept_rows(&self.installation));
            }
        }
        let mut page = CataloguePage::from_rows(String::from("0"), rows, query);
        if query.provider.is_none() {
            page.providers = providers;
        }
        page
    }

    fn run(&self, request: &InvokeRequest) -> Result<ReceiptStatus, ActionCallError> {
        if request.target.provider == host_steps::PROVIDER {
            return self.host_step(request);
        }
        // The name the step shows, if any: every member admits it only from
        // a program its installation lets post under other names.
        let invoker = self.invoker().with_appearance(request.appearance.clone());
        let mut member = self.lock();
        let route = resource_of(&request.target.provider, member.workspace())
            .filter(|route| member.has_record(route))
            .ok_or(ActionCallError::UnknownAction)?;
        let step = request.context.as_ref().and_then(AutomationStep::of);
        if let Some(step) = step.as_ref()
            && let Some((route, done)) = member.find_in_history(step, &self.actor())
        {
            return self.answer_from_history(member, &route, &request.target.address, step, &done);
        }
        let arguments = match &request.argument {
            InvokeArgument::Json(map) => map.clone(),
            InvokeArgument::None => Map::new(),
            InvokeArgument::Text(_) => {
                return Err(ActionCallError::InvalidRequest {
                    reason: "a program's step takes JSON arguments".into(),
                });
            }
        };
        let spec = self
            .outboxes
            .iter()
            .find(|spec| spec.app_id == route.app_id);
        let before: Vec<String> = spec
            .map(|spec| {
                member
                    .outbox(&route, &spec.field)
                    .into_iter()
                    .map(|row| row.id)
                    .collect()
            })
            .unwrap_or_default();
        let context = request
            .context
            .clone()
            .unwrap_or_default()
            .with_caused_by(self.cause());
        let output = match member.invoke(
            &route,
            &request.target.address,
            &arguments,
            &invoker,
            &context,
            request.confirmation.is_some(),
        ) {
            Ok(output) => output,
            Err(error) => return status_or_rejection(api_status(&error)),
        };
        let Some(spec) = spec else {
            return Ok(ReceiptStatus::Completed { output });
        };
        let queued: Vec<OutboxRow> = member
            .outbox(&route, &spec.field)
            .into_iter()
            .filter(|row| row.settled.is_none() && !before.contains(&row.id))
            .collect();
        drop(member);
        if queued.is_empty() {
            return Ok(ReceiptStatus::Completed { output });
        }
        self.perform(&route, &request.target.address, spec, queued, output)
    }

    /// Take a step the host answers itself: one for the person the program
    /// runs for, or one on what the program keeps. None of it is in a
    /// record's history, so a step the run takes again after a restart is
    /// asked of the host again under the same invocation and run step, which
    /// the host answers once ([`HostSteps`], [`Keeps`]).
    fn host_step(&self, request: &InvokeRequest) -> Result<ReceiptStatus, ActionCallError> {
        let invalid = |reason: String| ActionCallError::InvalidRequest { reason };
        let InvokeArgument::Json(arguments) = &request.argument else {
            return Err(invalid("a host step takes JSON arguments".into()));
        };
        let host = self.host.get();
        let keeps = self.keeps.get();
        let workspace = self.lock().workspace().to_owned();
        let asked = Asked::read(
            &request.target.address.action,
            arguments,
            AskedFor {
                person: host.map(|(person, _)| person.as_str()),
                workspace: &workspace,
                cause: StepCause {
                    installation: self.installation.clone(),
                    invocation: request.invocation,
                    event: self.cause().map(|cause| cause.event),
                },
                step: request.context.as_ref().and_then(AutomationStep::of),
                note: keeps.map_or(Value::Null, |keeps| keeps.note()),
                permits: keeps.map_or_else(Vec::new, |keeps| keeps.permits()),
            },
        )
        .map_err(invalid)?
        .ok_or(ActionCallError::UnknownAction)?;
        let answered = asked
            .answered_by(host.map(|(_, steps)| &**steps), keeps.map(|keeps| &**keeps))
            .ok_or(ActionCallError::UnknownAction)?;
        match answered {
            Ok(value) => Ok(ReceiptStatus::Completed {
                output: ActionOutput {
                    value,
                    resources: Vec::new(),
                },
            }),
            Err(HostStepError::Unavailable(reason)) => Err(ActionCallError::Unavailable { reason }),
            Err(HostStepError::Refused(reason)) => Ok(ReceiptStatus::Failed {
                error: ActionFailure {
                    code: host_steps::REFUSED.to_owned(),
                    message: reason,
                    details: None,
                    retryable: false,
                },
            }),
        }
    }

    /// Hand each newly queued entry to the connector and settle it, so the
    /// step's receipt carries what the external service answered.
    fn perform(
        &self,
        route: &DocumentRoute,
        address: &ActionAddress,
        spec: &OutboxSpec,
        queued: Vec<OutboxRow>,
        output: ActionOutput,
    ) -> Result<ReceiptStatus, ActionCallError> {
        let mut results = Vec::new();
        for row in queued {
            // The worker is waited on without holding the member, so
            // the runner's pump keeps taking in the session meanwhile.
            match spec.connector.perform(row.value) {
                Ok(value) => {
                    let outcome = json!({"state": "done", "result": value});
                    self.settle(route, address, spec, &row.id, &outcome)?;
                    results.push(value);
                }
                Err(PerformError::Refused(reason)) => {
                    let outcome = json!({"state": "failed", "result": {"reason": reason}});
                    self.settle(route, address, spec, &row.id, &outcome)?;
                    return Ok(ReceiptStatus::Failed {
                        error: ActionFailure {
                            code: "connector_refused".into(),
                            message: reason,
                            details: None,
                            retryable: false,
                        },
                    });
                }
                Err(PerformError::Unavailable(reason)) => {
                    return Err(ActionCallError::Unavailable { reason });
                }
            }
        }
        let value = if results.len() == 1 {
            results.pop()
        } else {
            Some(Value::Array(results))
        };
        Ok(ReceiptStatus::Completed {
            output: ActionOutput { value, ..output },
        })
    }

    /// Record how entry `entry` settled, as the program.
    fn settle(
        &self,
        route: &DocumentRoute,
        address: &ActionAddress,
        spec: &OutboxSpec,
        entry: &str,
        outcome: &Value,
    ) -> Result<(), ActionCallError> {
        let arguments = tilo_resource::outbox_settle_arguments(&spec.field, entry, outcome);
        let edit = ActionAddress::new(
            address.resource.clone(),
            tilo_resource::FIELD_EDIT.to_owned(),
        );
        let context = ActionContext::default().with_caused_by(self.cause());
        self.lock()
            .invoke(route, &edit, &arguments, &self.invoker(), &context, false)
            .map(drop)
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("the outbox entry was not settled: {error}"),
            })
    }

    /// What a step taken before answers: the settled results of the
    /// entries its change queued, else nothing beyond its item. An entry it
    /// queued that the connector could not take yet (a file whose bytes had
    /// not reached the host) is handed over now, so the retried run reads
    /// what the service answered rather than nothing.
    fn answer_from_history(
        &self,
        member: std::sync::MutexGuard<'_, P>,
        route: &DocumentRoute,
        address: &ActionAddress,
        step: &AutomationStep,
        done: &ActionContext,
    ) -> Result<ReceiptStatus, ActionCallError> {
        let Some(spec) = self
            .outboxes
            .iter()
            .find(|spec| spec.app_id == route.app_id)
        else {
            return Ok(ReceiptStatus::Completed {
                output: ActionOutput::default(),
            });
        };
        let mut queued = member.queued_in_history(route, step, &self.actor(), &spec.field);
        if let Some(item) = &done.item {
            queued.push(item.id.clone());
        }
        let rows: Vec<OutboxRow> = member
            .outbox(route, &spec.field)
            .into_iter()
            .filter(|row| queued.contains(&row.id))
            .collect();
        drop(member);
        let unsettled: Vec<OutboxRow> = rows
            .iter()
            .filter(|row| row.settled.is_none())
            .cloned()
            .collect();
        if !unsettled.is_empty() {
            return self.perform(route, address, spec, unsettled, ActionOutput::default());
        }
        let mut results: Vec<Value> = rows
            .into_iter()
            .filter_map(|row| row.settled?.get("result").cloned())
            .collect();
        let value = match results.len() {
            0 => None,
            1 => results.pop(),
            _ => Some(Value::Array(results)),
        };
        Ok(ReceiptStatus::Completed {
            output: ActionOutput {
                value,
                resources: Vec::new(),
            },
        })
    }
}

/// Whether `caller` is the program of `installation`.
fn is_bot(caller: &Caller, installation: &str) -> bool {
    caller.acting().bot_id() == Some(installation)
}

/// The program's record reads: what its runs read of the records it follows,
/// from the copies it holds ([`ApiMember::record`]). A run reads the
/// records as they were brought up to before it, with its own changes since.
struct BotReads<P: ApiMember> {
    member: Arc<Mutex<P>>,
    installation: String,
}

impl<P: ApiMember + 'static> ResourceReads for BotReads<P> {
    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        if !is_bot(caller, &self.installation) {
            return Err(ActionCallError::NotPermitted);
        }
        let member = self.member.lock_recover();
        let route = DocumentRoute {
            app_id: address.app.clone(),
            cluster_id: member.workspace().to_owned(),
            document: address.record.clone(),
        };
        match member.record(&route) {
            Ok(Some(document)) => Ok(ResourceCopy::Document(document)),
            Ok(None) => Ok(ResourceCopy::Absent),
            Err(reason) => Err(ActionCallError::Refused { reason }),
        }
    }

    /// Nothing but the run itself writes the copies while it reads them, so
    /// there is no write to wait for.
    fn watch(
        &self,
        caller: &Caller,
        _: &ResourceAddress,
        _: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        if is_bot(caller, &self.installation) {
            Ok(())
        } else {
            Err(ActionCallError::NotPermitted)
        }
    }
}

/// An API refusal as what `invoke` answers: a rejection is the call's
/// error, a failure its receipt.
fn status_or_rejection(status: ReceiptStatus) -> Result<ReceiptStatus, ActionCallError> {
    match status {
        ReceiptStatus::Rejected { error } => Err(error),
        status => Ok(status),
    }
}

fn update_of(invocation: InvocationId, status: &ReceiptStatus) -> Option<InvocationUpdate> {
    match status {
        ReceiptStatus::Completed { output } => Some(InvocationUpdate::Succeeded {
            invocation,
            output: output.clone(),
        }),
        ReceiptStatus::Failed { error } => Some(InvocationUpdate::Failed {
            invocation,
            error: error.clone(),
        }),
        ReceiptStatus::Cancelled { message } => Some(InvocationUpdate::Cancelled {
            invocation,
            message: message.clone(),
        }),
        ReceiptStatus::Accepted | ReceiptStatus::Rejected { .. } => None,
    }
}

impl<P: ApiMember + 'static> ActionProvider for ProgramApi<P> {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        if !self.its_own(caller) {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        Pending::ready(Ok(self.catalogue(&query)))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if !self.its_own(caller) {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        }
        let known = self.log.lock_recover().get(&request.invocation).cloned();
        if let Some(status) = known {
            return Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status,
            }));
        }
        let result = self.run(&request).map(|status| {
            self.log
                .lock_recover()
                .insert(request.invocation, status.clone());
            InvocationReceipt {
                invocation: request.invocation,
                status,
            }
        });
        Pending::ready(result)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        if !self.its_own(caller) {
            return None;
        }
        // Steps taken before a restart are found when the run invokes them
        // again ([`AutomationStep`]); the log knows this API's own.
        let status = self.log.lock_recover().get(id).cloned()?;
        update_of(*id, &status).map(|update| InvocationState::new(*id, update))
    }

    fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
        Some(Arc::new(BotReads {
            member: Arc::clone(&self.member),
            installation: self.installation.clone(),
        }))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(ActionCallError::NotCancellable {
            reason: "a program's step runs to its end".into(),
        }))
    }

    // `confirm` keeps the default: nobody answers for a program, so an action
    // that asks first is refused.
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;
