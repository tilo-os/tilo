//! A call that waits for its person's yes, kept by the device's one dispatch as its asked
//! invocation record.
//!
//! Every call an agent makes on this device reaches its workspace through the dispatch
//! ([`super::services::RuntimeActionProvider`]), under the agent's own key. One the workspace
//! refuses for want of the person's yes ([`ActionCallError::ConfirmationRequired`]) is written as
//! its asked record and waits there under way: the device's record of the invocation
//! ([`Invocations`]) reads it from here while it waits, and says it waits for its person. The
//! person answers it with `<record>#resolve`, from any device. An approve hands the call back
//! here, which sends it once more, byte for byte and under its id, as the agent that made it,
//! checked against the agent's grants as they stand then. A decline ends it, nothing of it run.
//! Stopping it before they answer withdraws the question ([`AskedStore::withdraw`]).
//!
//! The record keeps the whole call (target, arguments, id and caller) and the run it was made in
//! ([`Run`]), so a call that waits across the device's restart is sent again from it, in a run
//! admitted afresh for the same agent, under the key this device keeps for it.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::remote::asked::{
    self, AskedBy, AskedDispatch, AskedFacts, AskedRecords, AskedStore, Named,
};
use tilo_actions::remote::invocation_record::{Invocations, Route, take_back_run};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Completer,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, ReceiptStatus, pending,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

/// The name the store knows the device's dispatch by, across its restarts.
const DISPATCH: &str = "device";

/// The most calls whose end (declined, withdrawn, or refused when sent again) is kept for their
/// caller to read; the oldest goes first.
const ENDED_KEPT: usize = 1024;

/// The run a waiting call was made in, as its record keeps it: what admits a run of the same
/// asker again after the device starts again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "run", deny_unknown_fields)]
pub(crate) enum Run {
    /// An agent of the person's, by its id and the name it was given, in `workspace`.
    #[serde(rename_all = "camelCase")]
    Agent {
        workspace: String,
        agent: String,
        name: String,
    },
    /// The person's own call in `workspace`: from their shell (a question a program outside
    /// Tilo asks them, which the shell raises), or pressed in an app's window (`pressed`). Sent
    /// again as the person, through their own dispatch here.
    #[serde(rename_all = "camelCase")]
    Own {
        workspace: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pressed: Option<asked::Pressed>,
    },
}

impl Run {
    fn workspace(&self) -> &str {
        match self {
            Self::Agent { workspace, .. } | Self::Own { workspace, .. } => workspace,
        }
    }

    /// Who asks, as the person reads it, where the run says it: an agent is its own, whatever
    /// the provider said. The person's own call shows whoever its provider names (a program,
    /// a device, an app).
    fn asker(&self) -> Option<AskedBy> {
        match self {
            Self::Agent { agent, name, .. } => Some(AskedBy::Agent(Named {
                id: agent.clone(),
                name: name.clone(),
            })),
            Self::Own { .. } => None,
        }
    }
}

/// What admits `run` afresh: where its call is sent, and who it goes as there.
pub(crate) type Admits = Box<dyn Fn(&Run) -> Result<Route, String> + Send + Sync>;

/// A call that waits: the run it was made in, who follows it, where an approve sends it, and the
/// provider whose record holds the changes it already made.
struct Waiting {
    run: Run,
    caller: Caller,
    route: Route,
    at: String,
}

/// The asked records of the device's dispatch, and the calls that wait in them.
pub(crate) struct DeviceAsked {
    me: Weak<Self>,
    store: Arc<AskedStore>,
    records: Arc<dyn AskedRecords>,
    invocations: Arc<Invocations>,
    waiting: Mutex<HashMap<InvocationId, Waiting>>,
    /// How each call this dispatch keeps stands while nothing of it runs: waiting, declined,
    /// withdrawn, or refused when it was sent again; with who it runs as, who alone reads it.
    standing: Arc<Mutex<Standing>>,
    admits: Admits,
}

#[derive(Default)]
struct Standing {
    by_id: HashMap<InvocationId, (Caller, InvocationState)>,
    order: VecDeque<InvocationId>,
    /// Who waits for each one's next change.
    watching: HashMap<InvocationId, Vec<Completer<()>>>,
}

impl Standing {
    /// Have `state`'s invocation stand at it for `acting` to read, with nothing of it running.
    fn stand(standing: &Mutex<Self>, acting: &Caller, state: InvocationState) {
        let id = state.invocation();
        let mut standing = standing.lock_recover();
        if standing.by_id.insert(id, (acting.clone(), state)).is_none() {
            standing.order.push_back(id);
        }
        while standing.order.len() > ENDED_KEPT {
            if let Some(oldest) = standing.order.pop_front() {
                standing.by_id.remove(&oldest);
            }
        }
        let watching = standing.watching.remove(&id).unwrap_or_default();
        drop(standing);
        watching
            .into_iter()
            .for_each(|watcher| watcher.complete(()));
    }

    /// `id` no longer stands here: it runs where it was sent, and is read there.
    fn leave(standing: &Mutex<Self>, id: InvocationId) {
        let mut standing = standing.lock_recover();
        standing.by_id.remove(&id);
        standing.order.retain(|kept| *kept != id);
        let watching = standing.watching.remove(&id).unwrap_or_default();
        drop(standing);
        watching
            .into_iter()
            .for_each(|watcher| watcher.complete(()));
    }
}

impl DeviceAsked {
    /// The device's dispatch, writing its waiting calls through `store`, read through
    /// `invocations`, and admitting a run again with `admits`.
    pub(crate) fn new(
        store: Arc<AskedStore>,
        invocations: Arc<Invocations>,
        admits: Admits,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me: &Weak<Self>| {
            let dispatch: Weak<dyn AskedDispatch> = me.clone();
            Self {
                me: me.clone(),
                records: store.for_dispatch(DISPATCH, dispatch),
                store,
                invocations,
                waiting: Mutex::default(),
                standing: Arc::default(),
                admits,
            }
        })
    }

    /// Withdraw what waited before the device started again on a provider that ended with it:
    /// one the person's shell served over its link (a question a program outside Tilo asked
    /// through it), which nothing serves until a shell links again, and whose program's call
    /// ended with the device. A call to a provider the device serves itself (an app's, a
    /// shortcut's, an agent's) waits on.
    pub(crate) fn withdraw_what_ended(&self) {
        for (at, target) in self.store.asked_by(DISPATCH) {
            if runtime_collab::wire::shell::serves(&target.provider) {
                self.store.withdraw(&at, PROGRAM_GONE);
            }
        }
    }

    /// The store its asked records are in: what admits the approve it hands back.
    pub(crate) fn store(&self) -> &AskedStore {
        &self.store
    }

    /// The questions that wait for the person whose root key is `person`, oldest first: what
    /// `tilo.asking/waiting#list` answers, so a terminal answers them as any other client does.
    pub(crate) fn asked_of(&self, person: &[u8; 32]) -> Vec<asked::Asked> {
        self.records.asked_of(person)
    }

    /// Send the call `request` that `caller` made in `run` to `route`: one refused there for
    /// want of the person's yes is kept as its asked record, and waits there under way; anything
    /// else is answered as `route` answers it. The device's record of the invocation is
    /// `caller`'s, written before it is sent.
    pub(crate) fn send(
        self: &Arc<Self>,
        run: Run,
        caller: &Caller,
        route: Route,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (provider, acting) = route.clone();
        self.invocations.record(
            caller,
            &acting,
            &request,
            Some(run.workspace()),
            route.clone(),
        );
        let me = Arc::clone(self);
        let (call, caller) = (request.clone(), caller.clone());
        provider
            .invoke(&acting, request.clone())
            .map(move |answered| match refused_for_a_yes(&answered) {
                // Kept where it was sent, for its person to answer there: it goes on, and the
                // listing says it waits for them.
                None if answered
                    .as_ref()
                    .err()
                    .is_some_and(ActionCallError::awaits_person) =>
                {
                    me.invocations.note_waiting(&call.invocation);
                    answered
                }
                Some(facts) if call.approval().is_none() => me.keep(
                    Waiting {
                        run,
                        caller,
                        route,
                        at: request.target.provider,
                    },
                    &call,
                    facts,
                ),
                _ => answered,
            })
    }

    fn keep(
        &self,
        waiting: Waiting,
        request: &InvokeRequest,
        mut facts: AskedFacts,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let invocation = request.invocation;
        if let Some(by) = waiting.run.asker() {
            facts.by = by;
        }
        let (caller, acting) = (waiting.caller.clone(), waiting.route.1.clone());
        let workspace = waiting.run.workspace().to_owned();
        // What an approve sends it again with, there before its record is written.
        self.waiting.lock_recover().insert(invocation, waiting);
        let record = match self
            .records
            .write_asked(&workspace, &acting, request, &facts)
        {
            Ok(record) => record,
            Err(why) => {
                self.waiting.lock_recover().remove(&invocation);
                return Err(ActionCallError::Refused {
                    reason: format!(
                        "it waits for its person's yes, and could not be kept for them to \
                         answer: {why}"
                    ),
                });
            }
        };
        let state = InvocationState::new(invocation, record.waiting(invocation));
        let receipt = state.receipt();
        Standing::stand(&self.standing, &acting, state);
        // Read from here while it waits, and listed as waiting for its person: synchronously,
        // so the next read sees it.
        self.invocations.record(
            &caller,
            &acting,
            request,
            Some(&workspace),
            self.route(&acting),
        );
        self.invocations.note_waiting(&invocation);
        Ok(receipt)
    }

    /// This dispatch as where `acting`'s call runs while it waits here.
    fn route(&self, acting: &Caller) -> Route {
        let provider: Arc<dyn ActionProvider + Send + Sync> = match self.me.upgrade() {
            Some(me) => me,
            None => Arc::new(Gone),
        };
        (provider, acting.clone())
    }

    /// The call kept from before the device started again, in a run admitted afresh from its
    /// record for `restored`, who made it.
    fn admitted_again(
        &self,
        restored: &Caller,
        request: &InvokeRequest,
        context: &Value,
    ) -> Result<Waiting, ActionCallError> {
        let run: Run = serde_json::from_value(context.clone()).map_err(|_| not_waiting())?;
        let route = (self.admits)(&run).map_err(|why| ActionCallError::Refused {
            reason: format!("the run that asked could not be admitted again: {why}"),
        })?;
        // Only for the same asker, under the same key.
        if !same_asker(&route.1, restored) {
            return Err(not_waiting());
        }
        Ok(Waiting {
            run,
            caller: restored.clone(),
            route,
            at: request.target.provider.clone(),
        })
    }
}

/// Whether `a` and `b` are the same asker: the same person's, under the same key and agent.
fn same_asker(a: &Caller, b: &Caller) -> bool {
    a.user_public_key() == b.user_public_key()
        && a.device_public_key() == b.device_public_key()
        && a.acting().agent_id() == b.acting().agent_id()
}

/// What `answered` waits for, when it is a refusal for want of the person's yes.
fn refused_for_a_yes(answered: &Result<InvocationReceipt, ActionCallError>) -> Option<AskedFacts> {
    match answered {
        Err(ActionCallError::ConfirmationRequired(facts))
        | Ok(InvocationReceipt {
            status:
                ReceiptStatus::Rejected {
                    error: ActionCallError::ConfirmationRequired(facts),
                },
            ..
        }) => Some(facts.as_ref().clone()),
        _ => None,
    }
}

/// What a call that no longer waits here is refused another answer with.
fn not_waiting() -> ActionCallError {
    ActionCallError::Refused {
        reason: "The call no longer waits for this answer".into(),
    }
}

/// How `invocation` ends without running: `message` says why.
fn cancelled(invocation: InvocationId, message: &str) -> InvocationState {
    InvocationState::new(
        invocation,
        InvocationUpdate::Cancelled {
            invocation,
            message: Some(message.to_owned()),
        },
    )
}

impl AskedDispatch for DeviceAsked {
    fn context(&self, invocation: InvocationId) -> Value {
        self.waiting
            .lock_recover()
            .get(&invocation)
            .and_then(|waiting| serde_json::to_value(&waiting.run).ok())
            .unwrap_or(Value::Null)
    }

    fn readmit(
        &self,
        restored: &Caller,
        request: InvokeRequest,
        context: &Value,
    ) -> Result<(), ActionCallError> {
        let remembered = self.waiting.lock_recover().remove(&request.invocation);
        let Waiting {
            run, caller, route, ..
        } = match remembered {
            Some(waiting) => waiting,
            None => self.admitted_again(restored, &request, context)?,
        };
        // It runs where it is sent from now on, and is read there.
        self.invocations.record(
            &caller,
            &route.1,
            &request,
            Some(run.workspace()),
            route.clone(),
        );
        Standing::leave(&self.standing, request.invocation);
        let (provider, acting) = route;
        let (invocation, standing) = (request.invocation, Arc::downgrade(&self.standing));
        let (invocations, me) = (Arc::clone(&self.invocations), self.me.clone());
        provider.invoke(&acting, request).then(move |answered| {
            // Refused at its admission (the grants no longer reach it): it ran nothing, and its
            // caller reads why, here.
            let error = match answered {
                Some(
                    Ok(InvocationReceipt {
                        status: ReceiptStatus::Rejected { error },
                        ..
                    })
                    | Err(error),
                ) => error,
                None => ActionCallError::Unavailable {
                    reason: "the workspace stopped before it answered".into(),
                },
                Some(Ok(_)) => return,
            };
            let state = InvocationState::new(
                invocation,
                InvocationUpdate::Failed {
                    invocation,
                    error: ActionFailure {
                        code: error.code().to_owned(),
                        message: error.to_string(),
                        details: None,
                        retryable: false,
                    },
                },
            );
            let (Some(standing), Some(me)) = (standing.upgrade(), me.upgrade()) else {
                return;
            };
            Standing::stand(&standing, &acting, state);
            if let Some(recorded) = invocations.get(&caller, &invocation) {
                invocations.record(
                    &caller,
                    &acting,
                    &recorded.request,
                    recorded.workspace.as_deref(),
                    me.route(&acting),
                );
            }
        });
        Ok(())
    }

    fn declined(&self, _: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        let Some(waiting) = self.waiting.lock_recover().remove(&invocation) else {
            // Kept from before the device started again: nothing of it runs here to end.
            return Ok(());
        };
        Standing::stand(
            &self.standing,
            &waiting.route.1,
            cancelled(invocation, asked::DECLINED),
        );
        // What the run wrote before it asked is taken back, keyed by its invocation: the same
        // take-back `<record>#undo` runs, at the provider whose record holds it.
        let _ = take_back_run(&waiting.route, &waiting.at, None, &invocation.0.to_string());
        Ok(())
    }

    fn withdrawn(&self, _: &Caller, invocation: InvocationId, message: &str) {
        if let Some(waiting) = self.waiting.lock_recover().remove(&invocation) {
            Standing::stand(
                &self.standing,
                &waiting.route.1,
                cancelled(invocation, message),
            );
        }
    }
}

/// What a question whose program stopped waiting with the device says, once withdrawn.
pub(crate) const PROGRAM_GONE: &str = "The program that asked is no longer running";

/// What stopping a waiting call says it ended with.
const STOPPED: &str = "stopped before its person answered";

/// While a call waits here, the device's record of it reads and stops it here.
impl ActionProvider for DeviceAsked {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn invoke(
        &self,
        _: &Caller,
        _: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(&self, acting: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let standing = self.standing.lock_recover();
        let (runs_as, state) = standing.by_id.get(id)?;
        let state = same_asker(runs_as, acting).then(|| state.clone());
        drop(standing);
        state
    }

    fn next_change(&self, acting: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let mut standing = self.standing.lock_recover();
        let (runs_as, _) = standing.by_id.get(id)?;
        if !same_asker(runs_as, acting) {
            return None;
        }
        let (watcher, change) = pending();
        standing.watching.entry(*id).or_default().push(watcher);
        drop(standing);
        Some(change)
    }

    /// Stopping a call that waits withdraws its question: its person is asked no more.
    fn cancel(&self, _: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        if !self.waiting.lock_recover().contains_key(id) {
            return Pending::ready(Err(ActionCallError::NotCancellable {
                reason: "it has ended".into(),
            }));
        }
        let Some(at) = self.store.record(&id.0.to_string()) else {
            return Pending::ready(Err(ActionCallError::UnknownInvocation));
        };
        self.store.withdraw(&at, STOPPED);
        Pending::ready(Ok(()))
    }
}

/// Where a call is read once the dispatch that kept it stopped.
struct Gone;

impl ActionProvider for Gone {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn invoke(
        &self,
        _: &Caller,
        _: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

#[cfg(test)]
#[path = "asked_tests.rs"]
mod tests;
