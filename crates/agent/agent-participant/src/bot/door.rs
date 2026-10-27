//! The door a bot's runs act through.
//!
//! Every step of a bot's run is an invocation here, as the bot: the caller
//! must be this installation's `Caller::bot`. The change names the bot, its
//! installer, the invocation it came from and the run's cause. The replica
//! decides with the installer's standing bounded by the bot's grants, so a
//! bot never does more than its installer can now.
//!
//! A step its run already took (the host restarted mid-run) is found in the
//! record's history by its derived invocation id and observed instead of
//! taken again. An outside step completes when the outbox entries it
//! queued are settled: the door hands each entry to the connector's worker
//! and settles it as the bot with what came back, so the request, its
//! result and the settle are all the bot's, and the step's value is the
//! result the next step reads. A refusal settles failed; an unavailable
//! service leaves the entry pending and the step unavailable, so the run
//! is retried in order and the entry performed again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};
use tilo_actions::context::Cause;
use tilo_actions::door::{ActionAddress, DoorError};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueProvider,
    CatalogueQuery, CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{
    ActionContext, ActionFailure, ActionInvoker, ActionOutput, ActorKind, InvocationId,
    InvocationUpdate,
};
use tilo_browser_peer::DocumentRoute;

use super::item_json::OutboxRow;
use crate::participant::actions::{door_status, record_of, record_provider};
use tilo_lock::MutexRecover as _;

/// What the door needs of the bot's session and replicas.
pub trait DoorParticipant: Send {
    fn room(&self) -> &str;
    /// The held records and what each offers the bot now.
    fn offered(&self) -> Vec<(DocumentRoute, Vec<tilo_actions::door::ActionDescriptor>)>;
    /// Whether the record at `route` is held.
    fn holds(&self, route: &DocumentRoute) -> bool;
    /// Invoke as `invoker` within `context` and commit the change.
    /// `confirmed`: the run confirmed it, from the grant's advance consent,
    /// for an action whose contract asks for confirmation.
    ///
    /// # Errors
    /// The door's refusal or the action's failure.
    fn invoke(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &Map<String, Value>,
        invoker: &ActionInvoker,
        context: &ActionContext,
        confirmed: bool,
    ) -> Result<ActionOutput, DoorError>;
    /// The entry `actor` recorded for step `step` of run `run` in any held
    /// record.
    fn find_in_history(
        &self,
        step: &RunStep,
        actor: &str,
    ) -> Option<(DocumentRoute, ActionContext)>;
    /// The outbox stored at `field` of the record at `route`, in order.
    fn outbox(&self, route: &DocumentRoute, field: &str) -> Vec<OutboxRow>;
    /// The ids of the entries the change `actor` recorded for step `step`
    /// put into the outbox at `field` of the record at `route`.
    fn queued_in_history(
        &self,
        route: &DocumentRoute,
        step: &RunStep,
        actor: &str,
        field: &str,
    ) -> Vec<String>;
}

/// The context of the latest change in `doc` that `actor` recorded for step
/// `step`, with the change.
pub(crate) fn step_change(
    doc: &automerge::Automerge,
    step: &RunStep,
    actor: &str,
) -> Option<(automerge::Change, ActionContext)> {
    doc.get_changes(&[]).into_iter().rev().find_map(|change| {
        let context = ActionContext::from_change_message(change.message()?)?;
        let by = context
            .invoker
            .as_ref()
            .map(|invoker| invoker.actor.as_str());
        (RunStep::of(&context).as_ref() == Some(step) && by == Some(actor))
            .then(|| (change.clone(), context))
    })
}

/// The ids of the entries the change `actor` recorded for step `step` in
/// `doc` put into the outbox at `field`: those there after it and not
/// before it.
pub(crate) fn queued_by_step(
    doc: &automerge::Automerge,
    step: &RunStep,
    actor: &str,
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

/// Which step of which run a change was: the run's group and the step
/// facet every step's context carries. History keeps both, so a run
/// retried after a restart finds the steps it already took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunStep {
    /// The run id.
    pub run: String,
    /// The step's path.
    pub step: String,
}

impl RunStep {
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
            run: group.id.clone(),
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
    ///
    /// # Errors
    /// Why it was not performed.
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

/// The `ActionHost` one installation's runs act through.
pub struct BotDoor<P: DoorParticipant> {
    participant: Arc<Mutex<P>>,
    installation: String,
    installer: String,
    outboxes: Vec<OutboxSpec>,
    cause: Mutex<Option<Cause>>,
    /// Steps taken since the door was made, by invocation.
    ledger: Mutex<HashMap<InvocationId, ReceiptStatus>>,
}

impl<P: DoorParticipant> BotDoor<P> {
    /// The door of installation `installation`, installed by `installer`.
    pub fn new(
        participant: Arc<Mutex<P>>,
        installation: String,
        installer: String,
        outboxes: Vec<OutboxSpec>,
    ) -> Self {
        Self {
            participant,
            installation,
            installer,
            outboxes,
            cause: Mutex::new(None),
            ledger: Mutex::new(HashMap::new()),
        }
    }

    /// Forget the steps this door took. For a run whose changes the room
    /// refused part of: run again, it finds the steps whose changes stand
    /// in the records' history and takes the others anew, reading the room
    /// as it now is, instead of answering every step from what it did the
    /// first time.
    pub fn forget_steps(&self) {
        self.ledger.lock_recover().clear();
    }

    /// The cause every change of the next run records.
    pub fn set_cause(&self, cause: Option<Cause>) {
        *self.cause.lock_recover() = cause;
    }

    fn cause(&self) -> Option<Cause> {
        self.cause.lock_recover().clone()
    }

    fn actor(&self) -> String {
        format!("bot:{}", self.installation)
    }

    fn its_own(&self, caller: &Caller) -> bool {
        matches!(caller.principal(), CallerPrincipal::Bot(id) if *id == self.installation)
    }

    fn invoker(&self) -> ActionInvoker {
        ActionInvoker::actor(self.actor())
            .with_kind(ActorKind::Bot)
            .acting_for(self.installer.clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, P> {
        self.participant.lock_recover()
    }

    fn catalogue(&self, query: &CatalogueQuery) -> CataloguePage {
        let participant = self.lock();
        let room = participant.room().to_owned();
        let mut rows = Vec::new();
        let mut providers = Vec::new();
        for (route, offered) in participant.offered() {
            let provider = record_provider(&route);
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
                room: room.clone(),
                record_id: route.record.clone(),
                browsable: false,
            });
            rows.extend(offered.into_iter().map(|descriptor| {
                CatalogueRow::for_action(provider.clone(), route.app_id.clone(), descriptor)
            }));
        }
        drop(participant);
        let mut page = CataloguePage::from_rows(String::from("0"), rows, query);
        if query.provider.is_none() {
            page.providers = providers;
        }
        page
    }

    fn run(&self, request: &InvokeRequest) -> Result<ReceiptStatus, ActionHostError> {
        // The name the step shows, if any: every member admits it only from
        // a bot its installation lets post under other names.
        let invoker = self.invoker().with_appearance(request.appearance.clone());
        let mut participant = self.lock();
        let route = record_of(&request.target.provider, participant.room())
            .filter(|route| participant.holds(route))
            .ok_or(ActionHostError::UnknownAction)?;
        let step = request.context.as_ref().and_then(RunStep::of);
        if let Some(step) = step.as_ref()
            && let Some((route, done)) = participant.find_in_history(step, &self.actor())
        {
            return self.answer_from_history(
                participant,
                &route,
                &request.target.address,
                step,
                &done,
            );
        }
        let arguments = match &request.argument {
            InvokeArgument::Json(map) => map.clone(),
            InvokeArgument::None => Map::new(),
            InvokeArgument::Text(_) => {
                return Err(ActionHostError::InvalidRequest {
                    reason: "a bot's step takes JSON arguments".into(),
                });
            }
        };
        let spec = self
            .outboxes
            .iter()
            .find(|spec| spec.app_id == route.app_id);
        let before: Vec<String> = spec
            .map(|spec| {
                participant
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
        let output = match participant.invoke(
            &route,
            &request.target.address,
            &arguments,
            &invoker,
            &context,
            request.confirmation.is_some(),
        ) {
            Ok(output) => output,
            Err(error) => return status_or_rejection(door_status(&error)),
        };
        let Some(spec) = spec else {
            return Ok(ReceiptStatus::Completed { output });
        };
        let queued: Vec<OutboxRow> = participant
            .outbox(&route, &spec.field)
            .into_iter()
            .filter(|row| row.settled.is_none() && !before.contains(&row.id))
            .collect();
        drop(participant);
        if queued.is_empty() {
            return Ok(ReceiptStatus::Completed { output });
        }
        self.perform(&route, &request.target.address, spec, queued, output)
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
    ) -> Result<ReceiptStatus, ActionHostError> {
        let mut results = Vec::new();
        for row in queued {
            // The worker is waited on without holding the participant, so
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
                    return Err(ActionHostError::Unavailable { reason });
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

    /// Record how entry `entry` settled, as the bot.
    fn settle(
        &self,
        route: &DocumentRoute,
        address: &ActionAddress,
        spec: &OutboxSpec,
        entry: &str,
        outcome: &Value,
    ) -> Result<(), ActionHostError> {
        let arguments = ui::tilo_resource::outbox_settle_arguments(&spec.field, entry, outcome);
        let edit = ActionAddress::new(
            address.resource.clone(),
            ui::tilo_resource::FIELD_EDIT.to_owned(),
        );
        let context = ActionContext::default().with_caused_by(self.cause());
        self.lock()
            .invoke(route, &edit, &arguments, &self.invoker(), &context, false)
            .map(drop)
            .map_err(|error| ActionHostError::Unavailable {
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
        participant: std::sync::MutexGuard<'_, P>,
        route: &DocumentRoute,
        address: &ActionAddress,
        step: &RunStep,
        done: &ActionContext,
    ) -> Result<ReceiptStatus, ActionHostError> {
        let Some(spec) = self
            .outboxes
            .iter()
            .find(|spec| spec.app_id == route.app_id)
        else {
            return Ok(ReceiptStatus::Completed {
                output: ActionOutput::default(),
            });
        };
        let mut queued = participant.queued_in_history(route, step, &self.actor(), &spec.field);
        if let Some(item) = &done.item {
            queued.push(item.id.clone());
        }
        let rows: Vec<OutboxRow> = participant
            .outbox(route, &spec.field)
            .into_iter()
            .filter(|row| queued.contains(&row.id))
            .collect();
        drop(participant);
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

/// A door refusal as what `invoke` answers: a rejection is the call's
/// error, a failure its receipt.
fn status_or_rejection(status: ReceiptStatus) -> Result<ReceiptStatus, ActionHostError> {
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

impl<P: DoorParticipant + 'static> ActionHost for BotDoor<P> {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        if !self.its_own(caller) {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        Pending::ready(Ok(self.catalogue(&query)))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if !self.its_own(caller) {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        let known = self.ledger.lock_recover().get(&request.invocation).cloned();
        if let Some(status) = known {
            return Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status,
            }));
        }
        let result = self.run(&request).map(|status| {
            self.ledger
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
        // again ([`RunStep`]); the ledger knows this door's own.
        let status = self.ledger.lock_recover().get(id).cloned()?;
        update_of(*id, &status).map(|update| InvocationState::new(*id, update))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        tilo_actions::remote::Pending::ready(Err(ActionHostError::NotCancellable {
            reason: "a bot's step runs to its end".into(),
        }))
    }

    // `confirm` keeps the default: nobody answers for a bot, so an action
    // that asks first is refused.
}

#[cfg(test)]
#[path = "door_tests.rs"]
mod tests;
