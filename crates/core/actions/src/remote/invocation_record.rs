//! A device's one record of each invocation it was asked for, by the invocation's id: who asked,
//! what they asked, in which workspace, when, how it stands, and the provider it runs at.
//!
//! However the device is asked about an invocation afterwards (on the connection that started it,
//! from another device of the person, over `/v1` or MCP, or as one of [`PROVIDER`]'s actions),
//! it answers from this record, and the record asks the provider the call went to. A host that
//! hands a call on to another host records where it went ([`Invocations::record`] again), so
//! following, cancelling and waiting reach the host that runs it with no map of its own.
//!
//! [`PROVIDER`]: super::workspace_invocations::PROVIDER

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use super::workspace_invocations::{CANCEL, Origin, PROVIDER, RETRY, Spend, UNDO};
use super::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, InvocationReceipt, InvocationState,
    InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE, Pending, ReceiptStatus, RemoteActionTarget,
    ResourceWake, pending,
};
use crate::InvocationId;

/// Whether `caller` may write the resource an invocation was made from
/// ([`Recorded::origin`]): what a provider's own grant check answers at the moment it is
/// asked, so a person's access to that resource decides it then, never as it was.
pub type OriginWrites<'a> = dyn Fn(&Caller, &Origin) -> bool + 'a;

/// The most invocations a device keeps a record of; the oldest goes first.
const KEPT: usize = 4096;

/// Why an invocation is not taken back at a provider: the record there offers no history.
pub const NO_HISTORY: &str = "the record it changed offers no history to take back";

/// `state`, with what a secret answer said replaced by the word that it was given.
fn given(mut state: InvocationState) -> InvocationState {
    if let crate::InvocationUpdate::Succeeded { output, .. } = &mut state.update {
        *output = crate::ActionOutput {
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
    /// Its answer is a secret ([`crate::SECRET_ANSWER_TAG`]): the record keeps neither what it
    /// was asked with nor what it answered, only that it was answered.
    pub secret: bool,
    route: Route,
}

/// The invocations one device was asked for.
#[derive(Default)]
pub struct Invocations(Mutex<Kept>);

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
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
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
        let recorded = Recorded {
            caller: caller.clone(),
            asked: asked.clone(),
            request: request.clone(),
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
            secret: false,
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

    /// Note that `id` answers a secret ([`crate::SECRET_ANSWER_TAG`]): its record forgets what
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
        if !Self::may_manage(caller, &recorded, may_write_origin) {
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
                            output: crate::ActionOutput::default(),
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
                    output: crate::ActionOutput::default(),
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
        take_back_run(&recorded.route, &at, None, &run).then(move |done| {
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
        if recorded.request.signed.is_some() {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "it was asked for from another device: run it again from there".to_owned(),
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
                    output: crate::ActionOutput {
                        value: Some(serde_json::json!({
                            "invocation": fresh.0.to_string(),
                            "address": format!("{PROVIDER}/{}", fresh.0),
                            "retries": original.0.to_string(),
                        })),
                        ..crate::ActionOutput::default()
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

    /// Whether `caller` may stop `recorded` or run it again: it is its caller
    /// ([`Caller::continues`]), the person the agent that made it works for, or someone who may
    /// write the resource it was made from, as `may_write_origin` reads it now.
    fn may_manage(
        caller: &Caller,
        recorded: &Recorded,
        may_write_origin: &OriginWrites<'_>,
    ) -> bool {
        caller.continues(&recorded.caller)
            || agent_person_is(caller, &recorded.caller)
            || recorded
                .origin
                .as_ref()
                .is_some_and(|origin| may_write_origin(caller, origin))
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
        if !Self::may_manage(caller, &recorded, may_write_origin) {
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
///
/// The store's change history knows which changes a run made, so nothing is rebuilt here: the
/// record publishes the history verb `undo` with an `invocation` argument, and this runs it as
/// the route's caller. `#undo` and a decline both ask it, keyed by the invocation.
pub fn take_back_run(
    route: &Route,
    at: &str,
    root: Option<&str>,
    run: &str,
) -> Pending<Result<(), ActionCallError>> {
    let (provider, runs_as) = route.clone();
    let query = CatalogueQuery {
        provider: Some(at.to_owned()),
        needle: UNDO.to_owned(),
        max_results: MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    };
    let run = run.to_owned();
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
        provider.invoke(&runs_as, request).then(move |answered| {
            complete.complete(match answered {
                None => Err(ActionCallError::Unavailable {
                    reason: "the record did not answer".into(),
                }),
                Some(Ok(receipt)) => taken(receipt.status),
                Some(Err(error)) => Err(error),
            });
        });
    });
    answer
}

/// The `undo` action `rows` offer that takes an invocation back, its provider and address:
/// the one whose history lies at `root` when `root` names a prefix, preferring a verb that
/// names the invocation it takes.
fn undo_action(
    rows: &[super::CatalogueRow],
    root: Option<&str>,
) -> Option<(String, crate::ActionAddress)> {
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
            .get(crate::params::FROM_ARGUMENT)
            .and_then(crate::params::MadeFrom::from_json)
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
    use crate::remote::{
        CataloguePage, CatalogueQuery, InvokeArgument, PendingPoll, RemoteActionTarget,
    };
    use crate::{ActionOutput, InvocationUpdate};

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
                page.rows.push(super::super::CatalogueRow::for_action(
                    "terminal",
                    "terminal",
                    crate::ActionDescriptor {
                        address: crate::ActionAddress::new("tl:terminal:", UNDO),
                        semantic_identity: None,
                        resource_kind: "terminal".into(),
                        resource_label: "Terminal".into(),
                        action_label: "Undo".into(),
                        keywords: Vec::new(),
                        did: None,
                        icon: None,
                        description: None,
                        state: crate::ActionState::default(),
                        locus: crate::ExecutionLocus::ClientLocal,
                        kind: crate::OperationKind::Effect,
                        contract: crate::ActionContract::default(),
                        input: crate::InputSpec::None,
                        params: vec![crate::ActionParam::optional(
                            "invocation",
                            crate::ParamType::Text,
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
                address: crate::ActionAddress::new("terminal", "run"),
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
            address: crate::ActionAddress::new(id.0.to_string(), action),
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
        let agent = Caller::local_agent("scout").acting_for("ada");
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
        let runs_as = Caller::local_agent("host");
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
            address: crate::ActionAddress::new(id.0.to_string(), "get"),
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
