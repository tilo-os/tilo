//! A device's one record of each invocation it was asked for, by the invocation's id: who asked,
//! what they asked, in which workspace, when, how it stands, and the provider it runs at.
//!
//! However the device is asked about an invocation afterwards (on the connection that started it,
//! from another device of the person, over `/v1` or MCP, or as one of [`PROVIDER`]'s actions),
//! it answers from this record, and the record asks the provider the call went to. A host that
//! hands a call on to another host records where it went ([`Invocations::record`] again), so
//! following, cancelling and waiting reach the host that runs it with no map of its own.
//!
//! [`PROVIDER`]: tilo_action_services::workspace_invocations::PROVIDER

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use tilo_action_services::workspace_invocations::{CANCEL, Origin, PROVIDER, RETRY, Spend, UNDO};
use tilo_actions::InvocationId;
use tilo_actions::remote::secret_arguments::{
    SecretAnswers, SecretParameters, secret_arguments, without_secrets,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Completer,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE,
    MAX_REMOTE_WAIT, Pending, ReceiptStatus, RemoteActionTarget, ResourceWake, RunningTool,
    pending, settle_with,
};

/// Whether `caller` may write the resource an invocation was made from
/// ([`Recorded::origin`]): what a provider's own grant check answers at the moment it is
/// asked, so a person's access to that resource decides it then, never as it was.
pub type OriginWrites<'a> = dyn Fn(&Caller, &Origin) -> bool + 'a;

/// The most invocations a device keeps a record of; the oldest goes first.
const KEPT: usize = 4096;

/// Why an invocation is not run again from its record: it keeps none of what it was asked with
/// that may be secret.
pub const NOT_KEPT: &str = "what it was asked with is not kept with its record: ask for it again";

/// Why an invocation is not taken back at a provider: the record there offers no history.
pub const NO_HISTORY: &str = "the record it changed offers no history to take back";

/// `state`, with what a secret answer said replaced by the word that it was given.
fn given(mut state: InvocationState) -> InvocationState {
    if let tilo_actions::InvocationUpdate::Succeeded { output, .. } = &mut state.update {
        *output = tilo_actions::ActionOutput {
            value: Some(serde_json::json!({ "given": true })),
            resources: Vec::new(),
        };
    }
    state
}

/// Where an invocation runs: the provider, and the caller it runs as there.
pub type Route = (Arc<dyn ActionProvider + Send + Sync>, Caller);

/// One invocation as the device recorded it.
#[derive(Clone)]
pub struct Recorded {
    /// Who asked for it.
    pub caller: Caller,
    /// Who the workspace names as having asked: the same caller, or the agent the person's
    /// device handed the run to, which the caller alone does not say. Display only; who may
    /// follow and stop the call is [`Self::caller`].
    pub asked: Caller,
    pub request: InvokeRequest,
    /// The arguments [`Self::request`] was asked with that the record keeps none of, by name:
    /// those its action declares secret, or all where no descriptor of it was found.
    pub sealed: Vec<String>,
    /// The workspace it acts in, when it acts in one.
    pub workspace: Option<String>,
    /// When it was asked for and when it ended, in ms since the epoch.
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// How it stood when last asked.
    pub state: Option<InvocationState>,
    /// How often its state was seen to change: what a follower waits past.
    pub changes: u64,
    /// It was answered as waiting for its person, and has not ended since: the call goes on and
    /// ends once they decide.
    pub waiting: bool,
    /// The resource the call was made from, when it named one: the caller's `$from`, else the
    /// conversation its turn belongs to. Whoever may write it may stop the invocation or run it
    /// again.
    pub origin: Option<Origin>,
    /// The invocation this one runs again, when it is a retry: the same action, arguments and
    /// caller, under a new id.
    pub retries: Option<InvocationId>,
    /// What a model call it made cost, as the model endpoint wrote it.
    pub spend: Option<Spend>,
    /// What it changed was taken back, through its record's own history.
    pub undone: bool,
    /// Its answer is a secret ([`tilo_actions::SECRET_ANSWER_TAG`]): the record keeps neither what it
    /// was asked with nor what it answered, only that it was answered.
    pub secret: bool,
    route: Route,
}

/// A tool call an agent's session runs, as the device keeps it: how it stands, and
/// what its person stops.
struct ToolRun {
    invocation: InvocationId,
    inner: Mutex<RunState>,
    /// The person asked for it to stop, kept apart from the answer they hold: a call that
    /// answered in the same breath ends cancelled all the same.
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Everything one tool call's record answers, under one lock: its state, the stop the
/// session holds an answer to, and the readers waiting on it to move.
struct RunState {
    /// How it stands: running until the session says how it ended.
    state: InvocationState,
    /// Resolves the stop its person asked for, so the session answers the tool cancelled.
    stop: Option<tilo_actions::remote::Completer<Result<(), ActionCallError>>>,
    /// Readers waiting on it to move: told once it ended.
    changed: Vec<tilo_actions::remote::Completer<()>>,
}

impl ToolRun {
    fn started(invocation: InvocationId) -> Self {
        Self {
            invocation,
            inner: Mutex::new(RunState {
                state: InvocationState::new(
                    invocation,
                    tilo_actions::InvocationUpdate::Accepted { invocation },
                ),
                stop: None,
                changed: Vec::new(),
            }),
            stopped: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Whether its person asked for it to stop.
    fn stop_noted(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.stopped)
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, RunState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn state(&self) -> InvocationState {
        self.inner().state.clone()
    }

    /// The answer to the person's stop: ready at once once the call ended and there is nothing
    /// to stop.
    fn stopped(&self) -> Pending<Result<(), ActionCallError>> {
        let (complete, stopped) = pending();
        let mut inner = self.inner();
        if inner.state.update.is_terminal() || inner.stop.is_some() {
            drop(inner);
            complete.complete(Ok(()));
            return stopped;
        }
        inner.stop = Some(complete);
        stopped
    }

    /// Stop the call: the session answers it cancelled, and it ends.
    fn stop(&self) -> Pending<Result<(), ActionCallError>> {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        let asked = self.inner().stop.take();
        if let Some(asked) = asked {
            asked.complete(Ok(()));
        }
        Pending::ready(Ok(()))
    }

    /// The call ended: `stopped` where its person stopped it. A reader following it is told.
    fn finish(&self, stopped: bool) {
        let mut inner = self.inner();
        if inner.state.update.is_terminal() {
            return;
        }
        let invocation = self.invocation;
        inner.state = InvocationState::new(
            invocation,
            if stopped {
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("its person stopped it".to_owned()),
                }
            } else {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            },
        );
        let waiting = std::mem::take(&mut inner.changed);
        drop(inner);
        for changed in waiting {
            changed.complete(());
        }
    }

    /// Resolves once the call ends.
    fn changed(&self) -> Pending<()> {
        let (complete, changed) = pending();
        let mut inner = self.inner();
        if inner.state.update.is_terminal() {
            drop(inner);
            complete.complete(());
            return changed;
        }
        inner.changed.push(complete);
        changed
    }
}

/// The provider a tool call's record names: it answers how the call stands and stops it as its
/// person asks, and runs no action itself.
struct ToolRoute(std::sync::Arc<ToolRun>);

impl ActionProvider for ToolRoute {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: "a tool call is no action of a provider".to_owned(),
        }))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::Unavailable {
            reason: "a tool call is no action of a provider".to_owned(),
        }))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        Some(self.0.state())
    }

    fn next_change(&self, _caller: &Caller, _id: &InvocationId) -> Option<Pending<()>> {
        Some(self.0.changed())
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        self.0.stop()
    }
}

/// The invocations one device was asked for.
#[derive(Default)]
pub struct Invocations {
    kept: Mutex<Kept>,
    /// What each call's action declares secret, as the device's dispatch reads it from its
    /// registry of actions ([`Self::keep_no_secrets`]): no record here keeps those arguments.
    secrets: std::sync::OnceLock<SecretParameters>,
    /// Whether each call's action answers a secret, as the device's dispatch reads its
    /// declaration ([`Self::keep_secret_answers`]): no record here keeps its answer.
    answers: std::sync::OnceLock<SecretAnswers>,
}

#[derive(Default)]
struct Kept {
    by_id: HashMap<InvocationId, Recorded>,
    order: VecDeque<InvocationId>,
    /// Readers of a workspace's invocations, by the workspace: each woken once by the next
    /// invocation recorded there ([`Invocations::watch`]).
    watching: Vec<(String, ResourceWake)>,
}

impl Invocations {
    fn kept(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keep none of the arguments `secrets` names for a call's action, from now on: they reach
    /// the provider in the call alone, and one run again from its record is refused for want
    /// of them. Named once, by the device's dispatch.
    pub fn keep_no_secrets(&self, secrets: SecretParameters) {
        let _ = self.secrets.set(secrets);
    }

    /// Decide whether a call's action answers a secret ([`tilo_actions::SECRET_ANSWER_TAG`]) by
    /// `answers`, from now on: its declaration, as the device's dispatch reads it, keeps every
    /// record of the call from holding its answer, whoever asked and wherever it was dispatched.
    /// Named once, by the device's dispatch.
    pub fn keep_secret_answers(&self, answers: SecretAnswers) {
        let _ = self.answers.set(answers);
    }

    /// `request` as its record keeps it: without the arguments its action declares secret, or
    /// any where no descriptor of it is found, beside their names. A call of the vault's add by
    /// its address through the action registry carries the add's own arguments inside its
    /// `arguments`, and keeps none of the add's secrets either.
    fn journaled(&self, request: &InvokeRequest) -> (InvokeRequest, Vec<String>) {
        let names = self.secrets.get().map_or_else(Vec::new, |secrets| {
            secret_arguments(request, secrets(&request.target))
        });
        let (mut kept, taken) = without_secrets(request, &names);
        let mut sealed: Vec<String> = taken.keys().cloned().collect();
        if tilo_actions::remote::calls_by_address(request, "add")
            && let Some(secrets) = self.secrets.get()
            && let InvokeArgument::Json(arguments) = &mut kept.argument
            && let Some(serde_json::Value::Object(inner)) = arguments.get_mut("arguments")
        {
            let add = tilo_actions::remote::vault_target("add");
            let names = secrets(&add).unwrap_or_else(|| inner.keys().cloned().collect());
            for name in names {
                if inner.remove(&name).is_some() {
                    sealed.push(format!("arguments.{name}"));
                }
            }
        }
        (kept, sealed)
    }

    /// Record that `caller` asked for `request` in `workspace`, running at `route`, as `asked`
    /// names the asking. Recorded again, the invocation keeps who asked and when, and runs
    /// where it was handed on to.
    pub fn record(
        &self,
        caller: &Caller,
        asked: &Caller,
        request: &InvokeRequest,
        workspace: Option<&str>,
        route: Route,
    ) {
        self.recorded(caller, asked, request, workspace, route, None);
    }

    /// [`Self::record`] for an invocation that runs `retries` again: what a retry records beside
    /// what it runs.
    fn recorded(
        &self,
        caller: &Caller,
        asked: &Caller,
        request: &InvokeRequest,
        workspace: Option<&str>,
        route: Route,
        retries: Option<InvocationId>,
    ) {
        let id = request.invocation;
        let secret = self.answers_a_secret(request);
        let (mut journaled, sealed) = self.journaled(request);
        if secret {
            // What it was asked with may be as telling as what it answered; neither is kept.
            journaled.argument = InvokeArgument::None;
        }
        let recorded = Recorded {
            caller: caller.clone(),
            asked: asked.clone(),
            request: journaled,
            sealed,
            workspace: workspace.map(str::to_owned),
            started_ms: tilo_clock::unix_now_ms(),
            ended_ms: None,
            state: None,
            changes: 0,
            waiting: false,
            origin: origin_of(caller, request),
            retries,
            spend: None,
            undone: false,
            secret,
            route,
        };
        let mut kept = self.kept();
        if let Some(known) = kept.by_id.get_mut(&id) {
            known.route = recorded.route;
            return;
        }
        kept.by_id.insert(id, recorded);
        kept.order.push_back(id);
        while kept.order.len() > KEPT {
            if let Some(oldest) = kept.order.pop_front() {
                kept.by_id.remove(&oldest);
            }
        }
        let woken: Vec<ResourceWake> = match workspace {
            Some(workspace) => {
                let (woken, still) = std::mem::take(&mut kept.watching)
                    .into_iter()
                    .partition(|(watched, _)| watched == workspace);
                kept.watching = still;
                woken.into_iter().map(|(_, wake)| wake).collect()
            }
            None => Vec::new(),
        };
        drop(kept);
        for wake in woken {
            wake.wake();
        }
    }

    /// Whether `request`'s action answers a secret ([`Self::keep_secret_answers`]): kept out of
    /// every record of it, its answer and its arguments alike.
    fn answers_a_secret(&self, request: &InvokeRequest) -> bool {
        self.answers.get().map_or_else(
            || tilo_actions::remote::answers_secret(request),
            |answers| answers(request),
        )
    }

    /// Wake `wake` once `workspace`'s invocations next change: another is recorded there, or
    /// one that has not ended changes at the provider it runs at. What a reader that follows a
    /// workspace's invocations waits on before it reads them again; watched before that read,
    /// so nothing recorded between the two is missed.
    pub fn watch(&self, workspace: &str, wake: &ResourceWake) {
        let going: Vec<(InvocationId, Route)> = {
            let mut kept = self.kept();
            kept.watching.retain(|(_, waiting)| waiting.waiting());
            kept.watching.push((workspace.to_owned(), wake.clone()));
            kept.by_id
                .iter()
                .filter(|(_, recorded)| {
                    recorded.workspace.as_deref() == Some(workspace) && recorded.ended_ms.is_none()
                })
                .map(|(id, recorded)| (*id, recorded.route.clone()))
                .collect()
        };
        for (id, (provider, runs_as)) in going {
            if let Some(changed) = provider.next_change(&runs_as, &id) {
                let wake = wake.clone();
                changed.then(move |_| wake.wake());
            }
        }
    }

    /// `caller`'s session starts the tool `tool` under `id` in `workspace`: kept as a call of
    /// the agent's tools, so its person reads it while it runs and may stop it. The answer the
    /// session holds around the tool's own work says how that ended.
    #[must_use]
    pub fn tool_started(
        &self,
        caller: &Caller,
        workspace: &str,
        id: InvocationId,
        tool: &str,
    ) -> RunningTool {
        let run = std::sync::Arc::new(ToolRun::started(id));
        let request = InvokeRequest {
            invocation: id,
            target: tilo_action_services::agent_tools::target(
                tilo_action_services::agent_tools::CALL,
            ),
            argument: InvokeArgument::None,
            context: Some(tilo_actions::ActionContext {
                group: Some(tilo_actions::WorkGroup {
                    id: tool.to_owned(),
                    label: Some(tool.to_owned()),
                    kind: "agent-tool".to_owned(),
                    declared_id: None,
                    session: None,
                }),
                ..tilo_actions::ActionContext::default()
            }),
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let route: Route = (
            std::sync::Arc::new(ToolRoute(std::sync::Arc::clone(&run))),
            caller.clone(),
        );
        self.record(caller, caller, &request, Some(workspace), route);
        let ended = std::sync::Arc::clone(&run);
        RunningTool::new(run.stopped(), run.stop_noted(), move |stopped| {
            ended.finish(stopped);
        })
    }

    /// The record of `id`, as `caller` may read it: `None` when the device was not asked for it,
    /// or not by `caller` ([`Caller::continues`]): a call is followed and stopped by the caller
    /// that made it, never by another execution of the same person.
    #[must_use]
    pub fn get(&self, caller: &Caller, id: &InvocationId) -> Option<Recorded> {
        self.kept()
            .by_id
            .get(id)
            .filter(|recorded| caller.continues(&recorded.caller))
            .cloned()
    }

    /// How `id` stands, asked of the provider it runs at.
    #[must_use]
    pub fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.get(caller, id)?;
        self.refreshed(id)?.state
    }

    /// Every invocation this device was asked for in `workspace`, oldest first, its state asked
    /// again of the provider it runs at: what a listing of the workspace's invocations reads.
    ///
    /// This is the records alone, as the device keeps them. Which of them a caller may read is
    /// not decided here: the reader of a listing admits its caller and checks each record's
    /// address against what that caller's grants allow, before it reads this.
    #[must_use]
    pub fn listed(&self, workspace: &str) -> Vec<Recorded> {
        let ids: Vec<InvocationId> = {
            let kept = self.kept();
            kept.order
                .iter()
                .filter(|id| {
                    kept.by_id
                        .get(id)
                        .is_some_and(|recorded| recorded.workspace.as_deref() == Some(workspace))
                })
                .copied()
                .collect()
        };
        ids.iter().filter_map(|id| self.refreshed(id)).collect()
    }

    /// Note that the model call `id` made cost `spend`: what the model endpoint writes as it
    /// answers, so the listing says what a run paid for.
    pub fn note_spend(&self, id: &InvocationId, spend: Spend) {
        if let Some(recorded) = self.kept().by_id.get_mut(id) {
            recorded.spend = Some(spend);
        }
    }

    /// Forget what every record already kept of a secret answer: each of an action whose
    /// declaration answers a secret ([`Self::keep_secret_answers`]) keeps only that it was given.
    /// What the device's dispatch calls once it names them, so a record written before the
    /// declaration carried the tag holds none of the answer either.
    pub fn drop_secret_answers(&self) {
        let answers = self.answers.get();
        let mut kept = self.kept();
        for recorded in kept.by_id.values_mut() {
            let secret = recorded.secret
                || answers.is_some_and(|answers| answers(&recorded.request))
                || tilo_actions::remote::answers_secret(&recorded.request);
            if secret {
                recorded.secret = true;
                recorded.request.argument = InvokeArgument::None;
                recorded.state = recorded.state.take().map(given);
            }
        }
    }

    /// Note that `id` answers a secret ([`tilo_actions::SECRET_ANSWER_TAG`]): its record forgets what
    /// it was asked with, and keeps of its answer only that it was given. The secret reaches its
    /// caller in the receipt alone.
    pub fn keep_secret(&self, id: &InvocationId) {
        if let Some(recorded) = self.kept().by_id.get_mut(id) {
            recorded.secret = true;
            recorded.request.argument = InvokeArgument::None;
            recorded.state = recorded.state.take().map(given);
        }
    }

    /// Note that `id` was answered as waiting for its person: the call goes on and ends once
    /// they decide.
    pub fn note_waiting(&self, id: &InvocationId) {
        if let Some(recorded) = self.kept().by_id.get_mut(id) {
            recorded.waiting = true;
        }
    }

    /// `id`'s record, its state asked of the provider it runs at and kept.
    fn refreshed(&self, id: &InvocationId) -> Option<Recorded> {
        let (provider, runs_as) = self.kept().by_id.get(id)?.route.clone();
        let state = provider.observe(&runs_as, id);
        let ended = state
            .as_ref()
            .is_some_and(|state| state.update.is_terminal());
        let mut kept = self.kept();
        let recorded = kept.by_id.get_mut(id)?;
        if recorded.state.as_ref() != state.as_ref() {
            recorded.changes = recorded.changes.saturating_add(1);
        }
        if ended && recorded.ended_ms.is_none() {
            recorded.ended_ms = Some(tilo_clock::unix_now_ms());
        }
        recorded.state = if recorded.secret {
            state.map(given)
        } else {
            state
        };
        let recorded = recorded.clone();
        drop(kept);
        Some(recorded)
    }

    /// What resolves when `id` next changes, at the provider it runs at.
    #[must_use]
    pub fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let (provider, runs_as) = self.get(caller, id)?.route;
        provider.next_change(&runs_as, id)
    }

    /// The answer to `request` when it asks one of [`PROVIDER`]'s invocations to stop
    /// ([`CANCEL`]), to run again ([`RETRY`]), or to take back what it changed ([`UNDO`]): the
    /// invocation is the resource, by its id. `None` for any other request, a read included: a
    /// record is read through the record API, never as an action.
    ///
    /// A call is stopped, run again or taken back by its caller, by the person its agent works
    /// for, or by whoever `may_write_origin` says may write the resource it was made from.
    /// Anyone else is refused in words a person reads.
    ///
    /// Taking it back is the person's own act, not a run's: it rewrites the records under their
    /// name, where stopping a call or running it again only ends or starts one of theirs. So the
    /// person answers for a take-back as themselves, never through another live run of theirs,
    /// which borrows none of what this one wrote ([`Caller::continues`]).
    #[must_use]
    pub fn answer(
        self: &Arc<Self>,
        caller: &Caller,
        request: &InvokeRequest,
        may_write_origin: &OriginWrites<'_>,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        let target = &request.target;
        let action = target.address.action.as_str();
        if target.provider != PROVIDER || ![CANCEL, RETRY, UNDO].contains(&action) {
            return None;
        }
        let Some(id) = uuid::Uuid::parse_str(&target.address.resource)
            .ok()
            .map(InvocationId)
        else {
            return Some(Pending::ready(Err(ActionCallError::UnknownInvocation)));
        };
        let Some(recorded) = self.find(&id) else {
            return Some(Pending::ready(Err(ActionCallError::UnknownInvocation)));
        };
        if !Self::may_manage(caller, &recorded, may_write_origin, action == UNDO) {
            let verb = match action {
                RETRY => "run it again",
                UNDO => "take it back",
                _ => "stop it",
            };
            return Some(Pending::ready(Err(refused(verb, recorded.origin.as_ref()))));
        }
        if action == RETRY {
            return Some(Self::retry(self, &recorded, request.invocation));
        }
        if action == UNDO {
            let asked = request.invocation;
            let taken = Self::take_back_recorded(self, &recorded);
            let (complete, answer) = pending();
            taken.then(move |done| {
                complete.complete(match done {
                    Some(Ok(())) => Ok(InvocationReceipt {
                        invocation: asked,
                        status: ReceiptStatus::Completed {
                            output: tilo_actions::ActionOutput::default(),
                        },
                    }),
                    Some(Err(error)) => Err(error),
                    None => Err(ActionCallError::Unavailable {
                        reason: "the record did not answer".into(),
                    }),
                });
            });
            return Some(answer);
        }
        let (provider, runs_as) = recorded.route;
        let invocation = request.invocation;
        Some(provider.cancel(&runs_as, &id).map(move |stopped| {
            stopped.map(|()| InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput::default(),
                },
            })
        }))
    }

    /// Take back what one record's invocation changed, and say so of the record.
    fn take_back_recorded(
        records: &Arc<Self>,
        recorded: &Recorded,
    ) -> Pending<Result<(), ActionCallError>> {
        if recorded.undone {
            return Pending::ready(Err(already_taken_back()));
        }
        let id = recorded.request.invocation;
        let at = recorded.request.target.provider.clone();
        let run = id.0.to_string();
        let (complete, answer) = pending();
        let records = Arc::clone(records);
        take_back_run(&recorded.route, &at, None, &run, None).then(move |done| {
            let result = match done {
                None => Err(ActionCallError::Unavailable {
                    reason: "the record did not answer".into(),
                }),
                Some(Err(error)) => Err(error),
                Some(Ok(())) => {
                    records.mark_undone(&id);
                    Ok(())
                }
            };
            complete.complete(result);
        });
        answer
    }

    /// Note that what `id` changed was taken back.
    fn mark_undone(&self, id: &InvocationId) {
        if let Some(recorded) = self.kept().by_id.get_mut(id) {
            recorded.undone = true;
        }
    }

    /// Run `recorded`'s action again under a new invocation id, as the caller that made it and
    /// with the same arguments and on-behalf-of, at the provider it ran at. The new record says
    /// it retries the old one, and the answer to `asked` names the new record's address. A
    /// request its caller signed on another device binds its first id and is not signed again
    /// here: the caller asks for it again from there.
    fn retry(
        records: &Arc<Self>,
        recorded: &Recorded,
        asked: InvocationId,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // A tool call is the session's, not an action with an address to ask again.
        if recorded.request.target.provider == tilo_action_services::agent_tools::PROVIDER {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "a tool call is not run again from its record: its agent asks it again"
                    .to_owned(),
            }));
        }
        if recorded.request.signed.is_some() {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "it was asked for from another device: run it again from there".to_owned(),
            }));
        }
        // Run again without what it was asked with, it would not be the call it was.
        if !recorded.sealed.is_empty() {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: NOT_KEPT.to_owned(),
            }));
        }
        let original = recorded.request.invocation;
        let mut request = recorded.request.clone();
        request.invocation = InvocationId::new();
        let fresh = request.invocation;
        records.recorded(
            &recorded.caller,
            &recorded.asked,
            &request,
            recorded.workspace.as_deref(),
            recorded.route.clone(),
            Some(original),
        );
        let (provider, runs_as) = recorded.route.clone();
        let waiting = Arc::clone(records);
        provider.invoke(&runs_as, request).map(move |result| {
            if result
                .as_ref()
                .err()
                .is_some_and(ActionCallError::awaits_person)
            {
                waiting.note_waiting(&fresh);
            }
            result.map(|_| InvocationReceipt {
                invocation: asked,
                status: ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(serde_json::json!({
                            "invocation": fresh.0.to_string(),
                            "address": format!("{PROVIDER}/{}", fresh.0),
                            "retries": original.0.to_string(),
                        })),
                        ..tilo_actions::ActionOutput::default()
                    },
                },
            })
        })
    }

    /// The record of `id`, whoever asked for it: what stopping or running it again reads before
    /// it decides who may.
    fn find(&self, id: &InvocationId) -> Option<Recorded> {
        self.kept().by_id.get(id).cloned()
    }

    /// Whether `caller` may stop `recorded`, run it again or take it back: it is its caller
    /// ([`Caller::continues`]), the person the agent that made it works for, or someone who may
    /// write the resource it was made from, as `may_write_origin` reads it now.
    ///
    /// `takes_back` says the record's content is taken back, not only the call ended or started
    /// again: that is the person's own act, so their answer is never given through another live
    /// run of theirs than the one that made the call ([`another_run`]).
    fn may_manage(
        caller: &Caller,
        recorded: &Recorded,
        may_write_origin: &OriginWrites<'_>,
        takes_back: bool,
    ) -> bool {
        let person = |owner: &Caller| {
            agent_person_is(caller, owner) && !(takes_back && another_run(caller, recorded))
        };
        caller.continues(&recorded.caller)
            || person(&recorded.caller)
            // A session's call is made by its person's connection and asked as the agent the
            // session runs: the person it works for may stop it either way.
            || person(&recorded.asked)
            || recorded
                .origin
                .as_ref()
                .is_some_and(|origin| may_write_origin(caller, origin))
    }

    /// Stop every call still running in `workspace` that the agent `agent` made, as `caller`
    /// (the person the agent works for) asks: each at the provider it runs at, through
    /// [`Self::cancel`], and only those `caller` may manage.
    #[must_use]
    pub fn stop_agent(
        &self,
        caller: &Caller,
        workspace: &str,
        agent: &tilo_action_policy::MemberId,
        may_write_origin: &OriginWrites<'_>,
    ) -> Vec<Pending<Result<(), ActionCallError>>> {
        let ids: Vec<InvocationId> = {
            let kept = self.kept();
            kept.order
                .iter()
                .filter_map(|id| {
                    let recorded = kept.by_id.get(id)?;
                    if recorded.workspace.as_deref() != Some(workspace)
                        || recorded.ended_ms.is_some()
                        || !(recorded.caller.acting().member_id() == Some(agent)
                            || recorded.asked.acting().member_id() == Some(agent))
                    {
                        return None;
                    }
                    Some(*id)
                })
                .collect()
        };
        ids.into_iter()
            .filter_map(|id| self.cancel(caller, &id, may_write_origin))
            .collect()
    }

    /// Stop `id` at the provider it runs at, for a caller that may manage it. `None` when the
    /// device was asked for no such invocation.
    #[must_use]
    pub fn cancel(
        &self,
        caller: &Caller,
        id: &InvocationId,
        may_write_origin: &OriginWrites<'_>,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        let recorded = self.find(id)?;
        if !Self::may_manage(caller, &recorded, may_write_origin, false) {
            return Some(Pending::ready(Err(refused(
                "stop it",
                recorded.origin.as_ref(),
            ))));
        }
        let (provider, runs_as) = recorded.route;
        Some(provider.cancel(&runs_as, id))
    }
}

/// Take back what the run `run` changed, through the record's own history: `at` is the provider
/// whose record holds the changes, and `root` the address prefix of the history action there
/// when the provider keeps many records (a session's own services), `None` when it keeps one.
/// `step` names the one change of that record to take back, as the record's own history names
/// it; absent to take back the run's whole share of the record.
///
/// The store's change history knows which changes a run made, so nothing is rebuilt here: the
/// record publishes the history verb `undo` with an `invocation` argument, and this runs it as
/// the route's caller. `#undo` and a decline both ask it, keyed by the invocation. The answer
/// says how taking it back went, not only that the verb was admitted: a host that runs it apart
/// is followed until it ends.
pub fn take_back_run(
    route: &Route,
    at: &str,
    root: Option<&str>,
    run: &str,
    step: Option<&str>,
) -> Pending<Result<(), ActionCallError>> {
    let (provider, runs_as) = route.clone();
    let query = CatalogueQuery {
        provider: Some(at.to_owned()),
        needle: UNDO.to_owned(),
        max_results: MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    };
    let run = run.to_owned();
    let step = step.map(str::to_owned);
    let root = root.map(str::to_owned);
    let (complete, answer) = pending();
    provider.query(&runs_as, query).then(move |page| {
        let found = match page {
            None => Err(ActionCallError::Unavailable {
                reason: "the record did not answer".into(),
            }),
            Some(Err(error)) => Err(error),
            Some(Ok(page)) => {
                undo_action(&page.rows, root.as_deref()).ok_or_else(|| ActionCallError::Refused {
                    reason: NO_HISTORY.to_owned(),
                })
            }
        };
        let (token, address) = match found {
            Ok(found) => found,
            Err(error) => {
                complete.complete(Err(error));
                return;
            }
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("invocation".to_owned(), serde_json::Value::String(run));
        if let Some(step) = step {
            arguments.insert("step".to_owned(), serde_json::Value::String(step));
        }
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: token,
                address,
            },
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let following = Arc::clone(&provider);
        let across = runs_as.clone();
        provider.invoke(&runs_as, request).then(move |answered| {
            match answered {
                None => complete.complete(Err(ActionCallError::Unavailable {
                    reason: "the record did not answer".into(),
                })),
                Some(Err(error)) => complete.complete(Err(error)),
                // A record whose host keeps its history apart admits the verb before it runs it:
                // the caller hears how taking it back went once the changes are gone.
                Some(Ok(receipt)) => followed(following, across, receipt, complete),
            }
        });
    });
    answer
}

/// Complete `done` with how taking back `receipt` went, once what it admitted ended: a host that
/// only accepted the record's `undo` is followed at `provider`, for as long as it runs, until it
/// says how it went ([`tilo_actions::remote::settle_with`]). A provider that cannot say how it stands leaves it
/// running.
fn followed(
    provider: Arc<dyn ActionProvider + Send + Sync>,
    runs_as: Caller,
    receipt: InvocationReceipt,
    done: Completer<Result<(), ActionCallError>>,
) {
    if !matches!(receipt.status, ReceiptStatus::Accepted) || done.is_abandoned() {
        done.complete(taken(receipt.status));
        return;
    }
    let now = tilo_clock::now();
    let until = now.checked_add(MAX_REMOTE_WAIT).unwrap_or(now);
    settle_with(&provider, &runs_as, receipt.invocation, None, until).then(move |state| {
        match state.flatten() {
            // Still running when a wait ran out: follow it again, as long as it runs.
            Some(state) if !state.is_terminal() && tilo_clock::now() >= until => {
                followed(provider, runs_as, state.receipt(), done);
            }
            Some(state) => done.complete(taken(state.receipt().status)),
            None => done.complete(taken(ReceiptStatus::Accepted)),
        }
    });
}

/// The `undo` action `rows` offer that takes an invocation back, its provider and address:
/// the one whose history lies at `root` when `root` names a prefix, preferring a verb that
/// names the invocation it takes.
fn undo_action(
    rows: &[tilo_actions::remote::CatalogueRow],
    root: Option<&str>,
) -> Option<(String, tilo_actions::ActionAddress)> {
    let mut fallback = None;
    for row in rows {
        let Some(action) = &row.action else {
            continue;
        };
        if action.address.action != UNDO {
            continue;
        }
        if root.is_some_and(|root| !action.address.resource.starts_with(root)) {
            continue;
        }
        if action.params.iter().any(|param| param.name == "invocation") {
            return Some((row.provider.clone(), action.address.clone()));
        }
        fallback.get_or_insert_with(|| (row.provider.clone(), action.address.clone()));
    }
    fallback
}

/// How taking back ended.
fn taken(status: ReceiptStatus) -> Result<(), ActionCallError> {
    match status {
        ReceiptStatus::Completed { .. } => Ok(()),
        ReceiptStatus::Failed { error } => Err(ActionCallError::Refused {
            reason: error.message,
        }),
        ReceiptStatus::Rejected { error } => Err(error),
        ReceiptStatus::Cancelled { .. } => Err(ActionCallError::Refused {
            reason: "taking it back was stopped".into(),
        }),
        ReceiptStatus::Accepted => Err(ActionCallError::Unavailable {
            reason: "taking it back is still running".into(),
        }),
    }
}

/// Why an invocation whose changes are gone is not taken back again.
fn already_taken_back() -> ActionCallError {
    ActionCallError::Refused {
        reason: "what this invocation changed is already taken back".into(),
    }
}

/// The resource `caller` made `request` from, when the call named one: its `$from`, else the
/// conversation the caller's turn belongs to.
fn origin_of(caller: &Caller, request: &InvokeRequest) -> Option<Origin> {
    if let InvokeArgument::Json(arguments) = &request.argument
        && let Some(from) = arguments
            .get(tilo_actions::params::FROM_ARGUMENT)
            .and_then(tilo_actions::params::MadeFrom::from_json)
        && let Some(origin) = Origin::parse(&from.resource, Some(&from.kind))
    {
        return Some(origin);
    }
    caller
        .turn()
        .and_then(|turn| Origin::parse(&turn.conversation, None))
}

/// Whether `caller` is the person `owner` works for, when `owner` is an agent's caller.
fn agent_person_is(caller: &Caller, owner: &Caller) -> bool {
    let Some(person) = owner.acting_for_person() else {
        return false;
    };
    let acting = caller.acting();
    acting.person_id() == Some(person)
        && !acting.is_agent()
        && !acting.is_bot()
        && acting.app().is_none()
}

/// Whether `caller` is a live run of the person's other than the live run that made `recorded`'s
/// call: a second connection of theirs borrows none of what the first wrote, as it borrows none
/// of the invocations it made ([`Caller::continues`]). A call an agent's own caller made ran in
/// no run of theirs, so any of them answers for it.
fn another_run(caller: &Caller, recorded: &Recorded) -> bool {
    caller.execution_is_live()
        && recorded.caller.execution_is_live()
        && caller.execution_id() != recorded.caller.execution_id()
}

/// Why `caller` may not `verb` an invocation: who may, and the resource that decides it.
fn refused(verb: &str, origin: Option<&Origin>) -> ActionCallError {
    let may = match origin {
        Some(origin) => format!(
            "the caller who made this invocation, the person its agent works for, or someone \
             who may write {} may",
            origin.address()
        ),
        None => {
            "the caller who made this invocation, or the person its agent works for, may".to_owned()
        }
    };
    ActionCallError::Refused {
        reason: format!("only {may} {verb}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use tilo_actions::remote::{
        CataloguePage, CatalogueQuery, InvokeArgument, PendingPoll, RemoteActionTarget,
    };
    use tilo_actions::{ActionOutput, InvocationUpdate};

    /// A host that runs every invocation until it is stopped, and whose record takes a run's
    /// writes back through its own history `undo`.
    #[derive(Default)]
    struct Runs {
        stopped: AtomicBool,
        /// The writes still in effect, each by the invocation it was one of, oldest first.
        written: Mutex<Vec<(String, String)>>,
    }

    impl Runs {
        /// The writes the invocation `id` made, as the record keeps them.
        fn writes(&self, id: &InvocationId) -> Vec<String> {
            self.written
                .lock()
                .unwrap()
                .iter()
                .filter(|(of, _)| of == &id.0.to_string())
                .map(|(_, what)| what.clone())
                .collect()
        }
    }

    impl ActionProvider for Runs {
        fn query(
            &self,
            _: &Caller,
            query: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            let mut page = CataloguePage::default();
            if query.provider.as_deref() == Some("terminal") {
                page.rows
                    .push(tilo_actions::remote::CatalogueRow::for_action(
                        "terminal",
                        "terminal",
                        tilo_actions::ActionDescriptor {
                            address: tilo_actions::ActionAddress::new("tl:terminal:", UNDO),
                            semantic_identity: None,
                            resource_kind: "terminal".into(),
                            resource_label: "Terminal".into(),
                            action_label: "Undo".into(),
                            keywords: Vec::new(),
                            did: None,
                            icon: None,
                            description: None,
                            state: tilo_actions::ActionState::default(),
                            locus: tilo_actions::ExecutionLocus::ClientLocal,
                            kind: tilo_actions::OperationKind::Effect,
                            contract: tilo_actions::ActionContract::default(),
                            input: tilo_actions::InputSpec::None,
                            params: vec![tilo_actions::ActionParam::optional(
                                "invocation",
                                tilo_actions::ParamType::Text,
                                "The invocation whose changes are taken back.",
                            )],
                            view: None,
                        },
                    ));
            }
            Pending::ready(Ok(page))
        }

        fn invoke(
            &self,
            _: &Caller,
            request: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            if request.target.address.action != UNDO {
                return Pending::ready(Err(ActionCallError::UnknownAction));
            }
            let InvokeArgument::Json(arguments) = &request.argument else {
                return Pending::ready(Err(ActionCallError::InvalidRequest {
                    reason: "the undo names no invocation".into(),
                }));
            };
            let named = arguments
                .get("invocation")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            // The store's history knows which changes the run made: exactly those go.
            self.written.lock().unwrap().retain(|(of, _)| of != named);
            Pending::ready(Ok(InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput::default(),
                },
            }))
        }

        fn observe(&self, _: &Caller, id: &InvocationId) -> Option<InvocationState> {
            let update = if self.stopped.load(Ordering::SeqCst) {
                InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: ActionOutput::default(),
                }
            } else {
                InvocationUpdate::Accepted { invocation: *id }
            };
            Some(InvocationState::new(*id, update))
        }

        fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
            self.stopped.store(true, Ordering::SeqCst);
            Pending::ready(Ok(()))
        }
    }

    fn request() -> InvokeRequest {
        InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: "terminal".into(),
                address: tilo_actions::ActionAddress::new("terminal", "run"),
            },
            argument: InvokeArgument::Text("make".into()),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// The request of one of a record's actions: stopping it, running it again, taking it back.
    fn record_action(id: &InvocationId, action: &str) -> InvokeRequest {
        let mut request = request();
        request.invocation = InvocationId::new();
        request.target = RemoteActionTarget {
            provider: PROVIDER.into(),
            address: tilo_actions::ActionAddress::new(id.0.to_string(), action),
        };
        request.argument = InvokeArgument::None;
        request
    }

    /// Record two writes of the run `id` in `runs`' history, as an action it ran made them.
    fn made(runs: &Runs, id: &InvocationId, first: &str, second: &str) {
        let mut written = runs.written.lock().unwrap();
        written.push((id.0.to_string(), first.to_owned()));
        written.push((id.0.to_string(), second.to_owned()));
    }

    /// How `answer` took a record action: ready, without a provider to wait on.
    fn answered(
        invocations: &Arc<Invocations>,
        caller: &Caller,
        request: &InvokeRequest,
        never: &OriginWrites<'_>,
    ) -> Result<InvocationReceipt, ActionCallError> {
        invocations
            .answer(caller, request, never)
            .expect("the record answers its own action")
            .poll_ready()
            .ready()
    }

    trait Ready<T> {
        fn ready(self) -> T;
    }

    impl<T: std::fmt::Debug> Ready<T> for PendingPoll<T> {
        fn ready(self) -> T {
            match self {
                PendingPoll::Ready(value) => value,
                other => panic!("the answer was ready, not {other:?}"),
            }
        }
    }

    /// A tool call an agent's session runs is an invocation of the agent's: listed while it
    /// runs, and stopped when its person stops the agent, which the session answers cancelled.
    #[test]
    fn a_running_tool_call_is_listed_and_its_person_stops_it() {
        let invocations = Arc::new(Invocations::default());
        let root = [1u8; 32];
        let agent = Caller::delegated_agent("claude", root, [2u8; 32]).in_workspace("w1");
        let person = Caller::local_person(hex::encode(root)).in_workspace("w1");
        let mut run = invocations.tool_started(&agent, "w1", InvocationId::new(), "tilo_watch");

        let listed = invocations.listed("w1");
        assert_eq!(listed.len(), 1, "the agent's call is listed");
        assert_eq!(
            listed[0].asked.acting().member_id(),
            agent.acting().member_id(),
            "as the agent's"
        );
        assert!(
            !listed[0]
                .state
                .as_ref()
                .expect("a running call says so")
                .update
                .is_terminal(),
            "and it is still running"
        );

        // The person stops the agent: what it was running is stopped, and the session answers it
        // cancelled.
        let member = agent
            .acting()
            .member_id()
            .expect("an agent is a member")
            .clone();
        let never = |_: &Caller, _: &Origin| false;
        let stopping = invocations.stop_agent(&person, "w1", &member, &never);
        assert_eq!(stopping.len(), 1, "the stop reaches the running call");
        let mut stopping = stopping.into_iter().next().expect("one stop");
        assert!(
            matches!(stopping.poll_ready(), PendingPoll::Ready(Ok(()))),
            "the call is stopped"
        );
        assert!(matches!(
            run.stopped.poll_ready(),
            PendingPoll::Ready(Ok(()))
        ));
        run.ended(true);

        let settled = invocations.listed("w1");
        assert!(
            matches!(
                settled[0].state.as_ref().map(|state| &state.update),
                Some(InvocationUpdate::Cancelled { .. })
            ),
            "the call ended cancelled"
        );
        assert!(settled[0].ended_ms.is_some(), "and says when");
    }

    /// A call a person's own connection made for the agent it runs is the agent's too: stopping
    /// the agent stops it.
    #[test]
    fn stopping_an_agent_stops_a_call_its_session_made_as_the_person() {
        let invocations = Arc::new(Invocations::default());
        let root = [1u8; 32];
        let agent = Caller::delegated_agent("claude", root, [2u8; 32]).in_workspace("w1");
        let person = Caller::local_person(hex::encode(root)).in_workspace("w1");
        let runs = Arc::new(Runs::default());
        let request = request();
        let id = request.invocation;
        invocations.record(
            &person,
            &agent,
            &request,
            Some("w1"),
            (runs.clone(), agent.clone()),
        );

        let member = agent
            .acting()
            .member_id()
            .expect("an agent is a member")
            .clone();
        let never = |_: &Caller, _: &Origin| false;
        let stopping = invocations.stop_agent(&person, "w1", &member, &never);
        assert_eq!(stopping.len(), 1, "the agent's call is stopped");
        let mut stopping = stopping.into_iter().next().expect("one stop");
        assert!(matches!(stopping.poll_ready(), PendingPoll::Ready(Ok(()))));
        assert!(
            runs.stopped.load(Ordering::SeqCst),
            "the provider it runs at was told"
        );
        assert!(invocations.get(&person, &id).is_some());
    }

    /// A call is recorded as every call is, and its record keeps none of the arguments its
    /// action declares secret: not the secret, not the note, not a field's value. What is safe
    /// to say of it stays.
    #[test]
    fn a_call_is_recorded_without_what_its_action_declares_secret() {
        let invocations = Arc::new(Invocations::default());
        let params = [
            tilo_actions::ActionParam::required("kind", tilo_actions::ParamType::Text, "kind"),
            tilo_actions::ActionParam::required("title", tilo_actions::ParamType::Text, "title"),
            tilo_actions::ActionParam::required(
                "secret",
                tilo_actions::ParamType::Secret,
                "secret",
            ),
            tilo_actions::ActionParam::optional("notes", tilo_actions::ParamType::Secret, "notes"),
            tilo_actions::ActionParam::optional(
                "fields",
                tilo_actions::ParamType::Secret,
                "fields",
            ),
        ];
        let target = tilo_actions::remote::vault_target("add");
        let described = target.clone();
        // The registry's own `call` is described too: its address and arguments are no secrets.
        invocations.keep_no_secrets(Arc::new(move |asked: &RemoteActionTarget| {
            if asked.provider == tilo_actions::remote::ACTIONS_PROVIDER {
                return Some(Vec::new());
            }
            (*asked == described)
                .then(|| tilo_actions::remote::secret_arguments::secret_parameters(&params))
        }));
        let asked = Caller::local_person("ada").in_new_execution();
        let mut request = request();
        request.target = target;
        let serde_json::Value::Object(arguments) = serde_json::json!({
            "kind": "login",
            "belongsTo": ["https://bank.example"],
            "title": "Bank",
            "secret": "made-up-password-0451",
            "notes": "made-up-recovery-words",
            "fields": [{"label": "PIN", "value": "made-up-pin-7351"}],
        }) else {
            unreachable!("an object");
        };
        // Passwords' code calls the same add by its address, through the action registry.
        let mut called = request.clone();
        called.invocation = InvocationId::new();
        called.target = RemoteActionTarget {
            provider: tilo_actions::remote::ACTIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new("actions", "call"),
        };
        let mut by_address = serde_json::Map::new();
        by_address.insert("address".into(), "tilo.vault/vault#add".into());
        by_address.insert("arguments".into(), arguments.clone().into());
        called.argument = InvokeArgument::Json(by_address);
        request.argument = InvokeArgument::Json(arguments);
        let runs = Arc::new(Runs::default());
        invocations.record(&asked, &asked, &request, Some("w1"), (runs, asked.clone()));
        let runs = Arc::new(Runs::default());
        invocations.record(&asked, &asked, &called, Some("w1"), (runs, asked.clone()));
        let kept = invocations.get(&asked, &request.invocation).unwrap();
        let kept_call = invocations.get(&asked, &called.invocation).unwrap();
        let written = serde_json::to_string(&kept.request).unwrap()
            + &serde_json::to_string(&kept_call.request).unwrap()
            + &invocations
                .listed("w1")
                .iter()
                .map(|listed| serde_json::to_string(&listed.request).unwrap())
                .collect::<String>();
        for secret in [
            "made-up-password-0451",
            "made-up-recovery-words",
            "made-up-pin-7351",
        ] {
            assert!(!written.contains(secret), "{written}");
        }
        assert!(written.contains("https://bank.example"), "{written}");
        assert!(written.contains("Bank"), "{written}");
    }

    /// A call of an action described nowhere on the device is recorded with none of what it
    /// was asked with, its text included, only the names of what it left out; run again from
    /// its record, it is refused, saying to ask for it again.
    #[test]
    fn a_call_of_an_undescribed_action_is_recorded_without_its_arguments() {
        let invocations = Arc::new(Invocations::default());
        invocations.keep_no_secrets(Arc::new(|_: &RemoteActionTarget| None));
        let asked = Caller::local_person("ada").in_new_execution();
        let mut request = request();
        request.argument = InvokeArgument::Text("made-up-passphrase-3317".into());
        let id = request.invocation;
        let runs = Arc::new(Runs::default());
        invocations.record(&asked, &asked, &request, Some("w1"), (runs, asked.clone()));
        let kept = invocations.get(&asked, &id).unwrap();
        assert_eq!(kept.request.argument, InvokeArgument::None);
        assert_eq!(
            kept.sealed,
            [tilo_actions::remote::secret_arguments::TEXT_ARGUMENT]
        );
        let listed = invocations
            .listed("w1")
            .iter()
            .map(|listed| serde_json::to_string(&listed.request).unwrap())
            .collect::<String>();
        assert!(!listed.contains("made-up-passphrase-3317"), "{listed}");
        let never = |_: &Caller, _: &Origin| false;
        match answered(&invocations, &asked, &record_action(&id, RETRY), &never) {
            Err(ActionCallError::Refused { reason }) => assert_eq!(reason, NOT_KEPT),
            other => panic!("run again without what it was asked with: {other:?}"),
        }
    }

    /// A vault reveal and a cloud mount's sign-in answer secrets, asked of their provider or
    /// through the action registry by address, so their records keep only that they were
    /// answered.
    #[test]
    fn a_vault_reveal_is_recorded_as_answering_a_secret() {
        use tilo_actions::remote::{ACCOUNTS_PROVIDER, answers_secret, vault_target};
        let invocations = Arc::new(Invocations::default());
        let asked = Caller::local_person("ada").in_new_execution();
        let mut reveal = request();
        reveal.target = vault_target("reveal");
        let mut credential = request();
        credential.target = RemoteActionTarget {
            provider: ACCOUNTS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new("accounts", "cloud-credential"),
        };
        let serde_json::Value::Object(signing_in) = serde_json::json!({
            "account": "files-1",
        }) else {
            unreachable!("an object");
        };
        credential.argument = InvokeArgument::Json(signing_in);
        let mut called = request();
        called.target = RemoteActionTarget {
            provider: "tilo.actions".to_owned(),
            address: tilo_actions::ActionAddress::new("actions", "call"),
        };
        let serde_json::Value::Object(arguments) = serde_json::json!({
            "address": "tilo.workspace/workspace.permissions:w1/vault#reveal",
            "arguments": {"item": "vault.item:00"},
        }) else {
            unreachable!("an object");
        };
        called.argument = InvokeArgument::Json(arguments);
        let mut called_credential = called.clone();
        called_credential.invocation = InvocationId::new();
        let serde_json::Value::Object(signing_in_call) = serde_json::json!({
            "address": "tilo.accounts/accounts#cloud-credential",
            "arguments": {"account": "files-1"},
        }) else {
            unreachable!("an object");
        };
        called_credential.argument = InvokeArgument::Json(signing_in_call);
        let mut listed = called.clone();
        listed.invocation = InvocationId::new();
        let serde_json::Value::Object(listing) = serde_json::json!({
            "address": "tilo.workspace/workspace.permissions:w1/vault#list",
        }) else {
            unreachable!("an object");
        };
        listed.argument = InvokeArgument::Json(listing);
        for (request, secret) in [
            (&reveal, true),
            (&credential, true),
            (&called, true),
            (&called_credential, true),
            (&listed, false),
        ] {
            assert_eq!(answers_secret(request), secret);
            let runs = Arc::new(Runs::default());
            invocations.record(&asked, &asked, request, Some("w1"), (runs, asked.clone()));
            let kept = invocations.get(&asked, &request.invocation).unwrap();
            assert_eq!(kept.secret, secret, "{:?}", request.target);
            if secret {
                assert_eq!(
                    kept.request.argument,
                    InvokeArgument::None,
                    "{:?} keeps none of what it was asked with",
                    request.target
                );
            }
        }
    }

    #[test]
    fn an_invocation_takes_back_the_writes_its_run_made() {
        let invocations = Arc::new(Invocations::default());
        let never = |_: &Caller, _: &Origin| false;
        let asked = Caller::local_person("ada").in_new_execution();
        let request = request();
        let id = request.invocation;
        let runs = Arc::new(Runs::default());
        invocations.record(
            &asked,
            &asked,
            &request,
            Some("w1"),
            (runs.clone(), asked.clone()),
        );
        made(&runs, &id, "Shipping notes.", " Sent.");
        assert_eq!(runs.writes(&id).len(), 2, "the run made two writes");

        let undone = answered(&invocations, &asked, &record_action(&id, UNDO), &never)
            .expect("its caller takes it back");
        assert!(matches!(undone.status, ReceiptStatus::Completed { .. }));
        assert!(
            runs.writes(&id).is_empty(),
            "both writes the run made are taken back"
        );
        assert!(
            invocations.get(&asked, &id).unwrap().undone,
            "the record says what it changed was taken back"
        );
    }

    #[test]
    fn someone_who_may_not_manage_the_run_is_refused_in_words() {
        let invocations = Arc::new(Invocations::default());
        let never = |_: &Caller, _: &Origin| false;
        let asked = Caller::local_person("ada").in_new_execution();
        let request = request();
        let id = request.invocation;
        let runs = Arc::new(Runs::default());
        invocations.record(
            &asked,
            &asked,
            &request,
            Some("w1"),
            (runs.clone(), asked.clone()),
        );
        made(&runs, &id, "Shipping notes.", " Sent.");

        let stranger = Caller::local_person("bob").in_new_execution();
        let refused = answered(&invocations, &stranger, &record_action(&id, UNDO), &never)
            .expect_err("a stranger is refused");
        let ActionCallError::Refused { reason } = &refused else {
            panic!("a stranger is refused in words, not {refused:?}");
        };
        assert!(
            reason.starts_with("only the caller who made this invocation"),
            "{reason}"
        );
        assert!(
            reason.ends_with("take it back"),
            "the refusal names what they may not do: {reason}"
        );
        assert_eq!(runs.writes(&id).len(), 2, "nothing was taken back");
    }

    #[test]
    fn taking_an_invocation_back_twice_is_refused_in_words() {
        let invocations = Arc::new(Invocations::default());
        let never = |_: &Caller, _: &Origin| false;
        let asked = Caller::local_person("ada").in_new_execution();
        let request = request();
        let id = request.invocation;
        let runs = Arc::new(Runs::default());
        invocations.record(
            &asked,
            &asked,
            &request,
            Some("w1"),
            (runs.clone(), asked.clone()),
        );
        made(&runs, &id, "Shipping notes.", " Sent.");
        answered(&invocations, &asked, &record_action(&id, UNDO), &never)
            .expect("the first takes it back");

        let refused = answered(&invocations, &asked, &record_action(&id, UNDO), &never)
            .expect_err("the second is refused");
        let ActionCallError::Refused { reason } = &refused else {
            panic!("a second undo is a refusal in words, not {refused:?}");
        };
        assert_eq!(
            reason, "what this invocation changed is already taken back",
            "a person reads what stands"
        );
    }

    #[test]
    fn an_invocations_person_takes_back_a_run_of_its_agents() {
        let invocations = Arc::new(Invocations::default());
        let never = |_: &Caller, _: &Origin| false;
        let agent = Caller::local_agent("scout", "ada");
        let request = request();
        let id = request.invocation;
        let runs = Arc::new(Runs::default());
        invocations.record(
            &agent,
            &agent,
            &request,
            Some("w1"),
            (runs.clone(), agent.clone()),
        );
        made(&runs, &id, "Shipping notes.", " Sent.");

        let person = Caller::local_person("ada").in_new_execution();
        answered(&invocations, &person, &record_action(&id, UNDO), &never)
            .expect("the person the agent works for takes it back");
        assert!(
            runs.writes(&id).is_empty(),
            "both of the agent's writes went"
        );
    }

    /// A call one live run of a person made for its agent is taken back by the person as
    /// themselves, never through another live run of theirs: that run is a connection of its own.
    /// Stopping the call is not the person's own act, so it stays theirs from anywhere.
    #[test]
    fn another_live_run_of_the_person_does_not_take_its_agents_call_back() {
        let invocations = Arc::new(Invocations::default());
        let never = |_: &Caller, _: &Origin| false;
        let agent = Caller::local_agent("scout", "ada");
        let run = Caller::local_person("ada")
            .in_workspace("w1")
            .in_new_execution();
        let request = request();
        let id = request.invocation;
        let runs = Arc::new(Runs::default());
        invocations.record(
            &run,
            &agent,
            &request,
            Some("w1"),
            (runs.clone(), agent.clone()),
        );
        made(&runs, &id, "Shipping notes.", " Sent.");

        let other = Caller::local_person("ada")
            .in_workspace("w1")
            .in_new_execution();
        assert!(
            !other.continues(&run),
            "another execution is not its caller"
        );
        let refused = answered(&invocations, &other, &record_action(&id, UNDO), &never)
            .expect_err("another run of the person is refused");
        let ActionCallError::Refused { reason } = &refused else {
            panic!("another run of the person is refused in words, not {refused:?}");
        };
        assert!(
            reason.starts_with("only the caller who made this invocation"),
            "{reason}"
        );
        assert_eq!(runs.writes(&id).len(), 2, "nothing was taken back");

        let mut stopping = invocations
            .cancel(&other, &id, &never)
            .expect("the call is recorded");
        assert!(
            matches!(stopping.poll_ready(), PendingPoll::Ready(Ok(()))),
            "stopping it is not taken back, so another run may ask"
        );

        // The person themselves takes it back: their own caller, in no run of its own.
        let person = Caller::local_person("ada").in_workspace("w1");
        answered(&invocations, &person, &record_action(&id, UNDO), &never)
            .expect("the person takes it back");
        assert!(runs.writes(&id).is_empty(), "both writes went");
    }

    #[test]
    fn an_invocation_is_followed_and_stopped_where_it_was_handed_on_to() {
        let invocations = Arc::new(Invocations::default());
        let never = |_: &Caller, _: &Origin| false;
        let asked = Caller::local_person("ada").in_new_execution();
        let request = request();
        let id = request.invocation;
        let here: Arc<Runs> = Arc::default();
        invocations.record(&asked, &asked, &request, Some("w1"), (here, asked.clone()));
        // Handed on to the workspace's host, which runs it as the workspace's caller.
        let there: Arc<Runs> = Arc::default();
        let runs_as = Caller::local_agent("host", "ada");
        invocations.record(
            &asked,
            &asked,
            &request,
            Some("w1"),
            (Arc::clone(&there) as _, runs_as),
        );

        // Its caller follows it where it runs; someone else, or another execution of the same
        // person, reads nothing of it.
        let running = invocations
            .observe(&asked, &id)
            .expect("its caller follows it");
        assert!(!running.update.is_terminal());
        for stranger in [
            Caller::local_person("bob"),
            asked.clone().in_new_execution(),
        ] {
            assert!(invocations.observe(&stranger, &id).is_none());
            assert!(matches!(
                invocations
                    .cancel(&stranger, &id, &never)
                    .expect("the device knows it")
                    .poll_ready(),
                PendingPoll::Ready(Err(ActionCallError::Refused { .. }))
            ));
        }

        let stopped = invocations
            .cancel(&asked, &id, &never)
            .expect("its caller stops it");
        let mut stopped = std::pin::pin!(stopped);
        let answered = std::future::Future::poll(
            stopped.as_mut(),
            &mut std::task::Context::from_waker(std::task::Waker::noop()),
        );
        assert_eq!(answered, std::task::Poll::Ready(Some(Ok(()))));
        assert!(
            there.stopped.load(Ordering::SeqCst),
            "stopped where it runs"
        );
        let ended = invocations.observe(&asked, &id).unwrap();
        assert!(ended.update.is_terminal());
        let recorded = invocations.get(&asked, &id).unwrap();
        assert_eq!(recorded.workspace.as_deref(), Some("w1"));
        assert_eq!(recorded.caller, asked, "who asked stays who asked");
        assert!(recorded.ended_ms.is_some());
        assert_eq!(recorded.changes, 2);

        // A record is read through the record API, never as one of its actions: what a model
        // call it made cost is what the reading carries.
        let mut get = request.clone();
        get.invocation = InvocationId::new();
        get.target = RemoteActionTarget {
            provider: PROVIDER.into(),
            address: tilo_actions::ActionAddress::new(id.0.to_string(), "get"),
        };
        assert!(
            invocations.answer(&asked, &request, &never).is_none(),
            "not one of its actions"
        );
        assert!(
            invocations.answer(&asked, &get, &never).is_none(),
            "a record is no action's to read"
        );
        invocations.note_spend(
            &id,
            Spend {
                provider: Some("Models".into()),
                model: Some("judge-small".into()),
                input_tokens: 3,
                output_tokens: 2,
                cost_micros: 4321,
            },
        );
        let spent = invocations.get(&asked, &id).unwrap().spend.unwrap();
        assert_eq!(spent.cost_micros, 4321);
    }
}
