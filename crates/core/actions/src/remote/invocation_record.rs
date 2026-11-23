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

use super::workspace_invocations::{CANCEL, Origin, PROVIDER, RETRY, Spend};
use super::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, Pending, ReceiptStatus, ResourceWake,
};
use crate::InvocationId;

/// Whether `caller` may write the resource an invocation was made from
/// ([`Recorded::origin`]): what a provider's own grant check answers at the moment it is
/// asked, so a person's access to that resource decides it then, never as it was.
pub type OriginWrites<'a> = dyn Fn(&Caller, &Origin) -> bool + 'a;

/// The most invocations a device keeps a record of; the oldest goes first.
const KEPT: usize = 4096;

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
        recorded.state = state;
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
    /// ([`CANCEL`]) or to run again ([`RETRY`]): the invocation is the resource, by its id. `None`
    /// for any other request, a read included: a record is read through the record API, never as
    /// an action.
    ///
    /// A call is stopped or run again by its caller, by the person its agent works for, or by
    /// whoever `may_write_origin` says may write the resource it was made from. Anyone else is
    /// refused in words a person reads.
    #[must_use]
    pub fn answer(
        self: &Arc<Self>,
        caller: &Caller,
        request: &InvokeRequest,
        may_write_origin: &OriginWrites<'_>,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        let target = &request.target;
        if target.provider != PROVIDER || ![CANCEL, RETRY].contains(&target.address.action.as_str())
        {
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
            let verb = if target.address.action == RETRY {
                "run it again"
            } else {
                "stop it"
            };
            return Some(Pending::ready(Err(refused(verb, recorded.origin.as_ref()))));
        }
        if target.address.action == RETRY {
            return Some(Self::retry(self, &recorded, request.invocation));
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::remote::{
        CataloguePage, CatalogueQuery, InvokeArgument, PendingPoll, RemoteActionTarget,
    };
    use crate::{ActionOutput, InvocationUpdate};

    /// A host that runs every invocation until it is stopped.
    #[derive(Default)]
    struct Runs {
        stopped: AtomicBool,
    }

    impl ActionProvider for Runs {
        fn query(
            &self,
            _: &Caller,
            _: CatalogueQuery,
        ) -> Pending<Result<CataloguePage, ActionCallError>> {
            Pending::ready(Ok(CataloguePage::default()))
        }

        fn invoke(
            &self,
            _: &Caller,
            _: InvokeRequest,
        ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
            Pending::ready(Err(ActionCallError::UnknownAction))
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
