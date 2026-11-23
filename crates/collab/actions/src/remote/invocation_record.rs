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

use super::workspace_invocations::{CANCEL, GET, PROVIDER, RETRY};
use super::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvocationState, InvokeRequest,
    Pending, ReceiptStatus,
};
use crate::InvocationId;

/// The most invocations a device keeps a record of; the oldest goes first.
const KEPT: usize = 4096;

/// Where an invocation runs: the provider, and the caller it runs as there.
pub type Route = (Arc<dyn ActionProvider + Send + Sync>, Caller);

/// One invocation as the device recorded it.
#[derive(Clone)]
pub struct Recorded {
    /// Who asked for it.
    pub caller: Caller,
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
    route: Route,
}

/// The invocations one device was asked for.
#[derive(Default)]
pub struct Invocations(Mutex<Kept>);

#[derive(Default)]
struct Kept {
    by_id: HashMap<InvocationId, Recorded>,
    order: VecDeque<InvocationId>,
}

impl Invocations {
    fn kept(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record that `caller` asked for `request` in `workspace`, running at `route`. Recorded
    /// again, the invocation keeps who asked and when, and runs where it was handed on to.
    pub fn record(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        workspace: Option<&str>,
        route: Route,
    ) {
        let id = request.invocation;
        let recorded = Recorded {
            caller: caller.clone(),
            request: request.clone(),
            workspace: workspace.map(str::to_owned),
            started_ms: tilo_clock::unix_now_ms(),
            ended_ms: None,
            state: None,
            changes: 0,
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
        drop(kept);
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
        let (provider, runs_as) = self.get(caller, id)?.route;
        let state = provider.observe(&runs_as, id)?;
        let mut kept = self.kept();
        if let Some(recorded) = kept.by_id.get_mut(id) {
            if recorded.state.as_ref() != Some(&state) {
                recorded.changes = recorded.changes.saturating_add(1);
            }
            if state.update.is_terminal() && recorded.ended_ms.is_none() {
                recorded.ended_ms = Some(tilo_clock::unix_now_ms());
            }
            recorded.state = Some(state.clone());
        }
        drop(kept);
        Some(state)
    }

    /// What resolves when `id` next changes, at the provider it runs at.
    #[must_use]
    pub fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let (provider, runs_as) = self.get(caller, id)?.route;
        provider.next_change(&runs_as, id)
    }

    /// The answer to `request` when it asks one of [`PROVIDER`]'s invocations how it stands
    /// ([`GET`]), to stop ([`CANCEL`]) or to run again ([`RETRY`]): the invocation is the
    /// resource, by its id. `None` for any other request.
    #[must_use]
    pub fn answer(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<Pending<Result<InvocationReceipt, ActionCallError>>> {
        let target = &request.target;
        if target.provider != PROVIDER
            || ![GET, CANCEL, RETRY].contains(&target.address.action.as_str())
        {
            return None;
        }
        let invocation = request.invocation;
        let receipt = move |status| Ok(InvocationReceipt { invocation, status });
        let Some(id) = uuid::Uuid::parse_str(&target.address.resource)
            .ok()
            .map(InvocationId)
            .filter(|id| self.get(caller, id).is_some())
        else {
            return Some(Pending::ready(Err(ActionCallError::UnknownInvocation)));
        };
        if target.address.action == CANCEL {
            let stopping = self.cancel(caller, &id)?;
            return Some(stopping.map(move |stopped| {
                stopped.and_then(|()| {
                    receipt(ReceiptStatus::Completed {
                        output: crate::ActionOutput::default(),
                    })
                })
            }));
        }
        if target.address.action == RETRY {
            return Some(self.retry(caller, &id, invocation));
        }
        let state = self.observe(caller, &id);
        let recorded = self.get(caller, &id)?;
        let value = serde_json::json!({
            "invocation": id.0.to_string(),
            "target": recorded.request.target,
            "workspace": recorded.workspace,
            "started_ms": recorded.started_ms,
            "ended_ms": recorded.ended_ms,
            "changes": recorded.changes,
            "state": state,
        });
        Some(Pending::ready(receipt(ReceiptStatus::Completed {
            output: crate::ActionOutput {
                value: Some(value),
                ..crate::ActionOutput::default()
            },
        })))
    }

    /// Run `id` again: its request, under a new id, as the caller that asked for it, at the
    /// provider it ran at. The new invocation is recorded like the first, and the receipt (as
    /// `asked`, the invocation that asked for the run) names it, so the one who retried follows
    /// it. A request its caller signed on another device binds its first id, and is not signed
    /// again here: the caller asks for it again from there.
    fn retry(
        &self,
        caller: &Caller,
        id: &InvocationId,
        asked: InvocationId,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Some(recorded) = self.get(caller, id) else {
            return Pending::ready(Err(ActionCallError::UnknownInvocation));
        };
        if recorded.request.signed.is_some() {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "it was asked for from another device: run it again from there".to_owned(),
            }));
        }
        let (provider, runs_as) = recorded.route.clone();
        let again = InvokeRequest {
            invocation: InvocationId::new(),
            ..recorded.request.clone()
        };
        let new = again.invocation;
        self.record(
            &recorded.caller,
            &again,
            recorded.workspace.as_deref(),
            (Arc::clone(&provider), runs_as.clone()),
        );
        provider.invoke(&runs_as, again).map(move |admitted| {
            admitted.map(|_| InvocationReceipt {
                invocation: asked,
                status: ReceiptStatus::Completed {
                    output: crate::ActionOutput {
                        value: Some(serde_json::json!({ "invocation": new.0.to_string() })),
                        ..crate::ActionOutput::default()
                    },
                },
            })
        })
    }

    /// Stop `id` at the provider it runs at.
    #[must_use]
    pub fn cancel(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        let (provider, runs_as) = self.get(caller, id)?.route;
        Some(provider.cancel(&runs_as, id))
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
        let invocations = Invocations::default();
        let asked = Caller::local_person("ada").in_new_execution();
        let request = request();
        let id = request.invocation;
        let here: Arc<Runs> = Arc::default();
        invocations.record(&asked, &request, Some("w1"), (here, asked.clone()));
        // Handed on to the workspace's host, which runs it as the workspace's caller.
        let there: Arc<Runs> = Arc::default();
        let runs_as = Caller::local_agent("host");
        invocations.record(
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
            assert!(invocations.cancel(&stranger, &id).is_none());
        }

        let stopped = invocations
            .cancel(&asked, &id)
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

        // Asked as an action of the invocation, by its id: its caller reads it, nobody else.
        let mut get = request.clone();
        get.invocation = InvocationId::new();
        get.target = RemoteActionTarget {
            provider: PROVIDER.into(),
            address: crate::ActionAddress::new(id.0.to_string(), GET),
        };
        assert!(
            invocations.answer(&asked, &request).is_none(),
            "not one of its actions"
        );
        let read = invocations.answer(&asked, &get).unwrap().poll_ready();
        let PendingPoll::Ready(Ok(InvocationReceipt {
            status: ReceiptStatus::Completed { output },
            ..
        })) = read
        else {
            panic!("its caller reads it");
        };
        let value = output.value.unwrap();
        assert_eq!(value["workspace"], "w1", "{value}");
        assert_eq!(value["state"]["update"]["state"], "succeeded", "{value}");
        assert!(matches!(
            invocations
                .answer(&Caller::local_person("bob"), &get)
                .unwrap()
                .poll_ready(),
            PendingPoll::Ready(Err(ActionCallError::UnknownInvocation))
        ));
    }
}
