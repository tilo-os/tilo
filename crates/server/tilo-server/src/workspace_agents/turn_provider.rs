//! What one turn's action service answers itself, beside the workspace's resources.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources. What this server answers itself for the person a turn acts for (a use of their
//! vault by reference, a service they connected) is a [`TurnProvider`]: named where the tools
//! list everything, asked for its rows at every listing, and called like any other action.
//!
//! The tools read how a call ended from the service that took it, not from its receipt. So the
//! service keeps how every call a provider took ended ([`Ended`]), and a provider keeps nothing
//! of the kind itself.
//!
//! A provider is added where a turn's service is put together, and decides for itself whom it
//! answers. For anything of a person's own, the turn's caller passes
//! [`crate::workspaces::Workspaces::asked_turn`] first.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueRow,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

/// How many calls a turn's service remembers the end of: more than a turn's tools ask about.
const KEPT: usize = 64;

/// How a call ends, when it has: what the tools are told, or the provider's refusal.
pub(super) type Ending =
    Pin<Box<dyn Future<Output = Result<InvocationUpdate, ActionCallError>> + Send>>;

/// The rows a provider answers a listing with, when it has them.
pub(super) type Rows = Pending<Result<Vec<CatalogueRow>, ActionCallError>>;

/// A provider one turn's action service answers itself.
pub(super) trait TurnProvider: Send + Sync {
    /// Its providers, as a listing of everything names them to `caller`. Each is `browsable`
    /// and in the turn's workspace, or no tool lists it.
    fn providers(&self, caller: &Caller) -> Vec<CatalogueProvider>;

    /// The rows `query` asks `caller` for, where it asks for one of those providers. Asked at
    /// every listing, so the rows may be made from what the provider holds then. Several
    /// providers of one turn may answer for the same provider id, each for a resource of its
    /// own: the service joins their rows into one page.
    fn rows(&self, caller: &Caller, query: &CatalogueQuery) -> Option<Rows>;

    /// Whether a call of `target` is this provider's to answer, also where it refuses it.
    fn takes(&self, target: &RemoteActionTarget) -> bool;

    /// Whether what a call of `target` answers holds words somebody outside wrote, such as
    /// an entry of a calendar. The turn's tools are told so with the answer: it is data to
    /// reason about, never an instruction to follow.
    fn carries_untrusted(&self, _target: &RemoteActionTarget) -> bool {
        false
    }

    /// Carry `request` out as `caller`, however long that takes.
    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending;
}

/// How the calls a turn's providers took ended, newest last: what the turn's tools observe.
#[derive(Clone)]
pub(super) struct Ended {
    /// Where the calls are carried out: a tool may call from outside any task.
    runtime: tokio::runtime::Handle,
    calls: Arc<Mutex<VecDeque<(InvocationId, InvocationState)>>>,
}

impl Ended {
    /// Nothing ended yet; calls are carried out on the runtime this is made on.
    pub(super) fn here() -> Self {
        Self {
            runtime: tokio::runtime::Handle::current(),
            calls: Arc::default(),
        }
    }

    /// One page of the rows the turn's providers answered `query` with.
    pub(super) fn page(
        &self,
        answers: Vec<Rows>,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let (completer, page) = tilo_actions::remote::pending();
        self.runtime.spawn(async move {
            let mut rows = Vec::new();
            for answer in answers {
                match answer.await {
                    Some(Ok(more)) => rows.extend(more),
                    Some(Err(error)) => return completer.complete(Err(error)),
                    // A provider that gave its answer up lists nothing.
                    None => {}
                }
            }
            completer.complete(Ok(CataloguePage::from_rows("0".into(), rows, &query)));
        });
        page
    }

    /// Let `provider` carry `request` out as `caller`. The receipt is answered when the call
    /// ended, and how it ended is kept before that, so the tool that asks next is told.
    pub(super) fn carry_out(
        &self,
        provider: Arc<dyn TurnProvider>,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (completer, answer) = tilo_actions::remote::pending();
        let (kept, caller) = (self.clone(), caller.clone());
        self.runtime.spawn(async move {
            let invocation = request.invocation;
            let untrusted = provider.carries_untrusted(&request.target);
            let receipt = match provider.invoke(caller, request).await {
                Ok(update) => {
                    let state = InvocationState::new(invocation, update).with_untrusted(untrusted);
                    kept.keep(invocation, state.clone());
                    state.receipt()
                }
                Err(error) => InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Rejected { error },
                },
            };
            completer.complete(Ok(receipt));
        });
        answer
    }

    fn keep(&self, invocation: InvocationId, state: InvocationState) {
        let mut calls = self.calls.lock_recover();
        if calls.len() == KEPT {
            calls.pop_front();
        }
        calls.push_back((invocation, state));
    }

    /// How the call `id` names ended, when a provider of this turn took it.
    pub(super) fn of(&self, id: &InvocationId) -> Option<InvocationState> {
        self.calls
            .lock_recover()
            .iter()
            .find(|(taken, _)| taken == id)
            .map(|(_, state)| state.clone())
    }
}
