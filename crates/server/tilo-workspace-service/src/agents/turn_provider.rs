//! What one turn's action service answers itself, beside the workspace's resources.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources. What this server answers itself for the person a turn acts for (a use of their
//! vault by reference, a service they connected) is a [`TurnProvider`]: named where the tools
//! list everything, asked for its rows at every listing, and called like any other action.
//!
//! The tools read how a call ended from the service that took it, not from its receipt. So the
//! service keeps how every call a provider took ended ([`OwnAnswers`]), and a provider keeps nothing
//! of the kind itself.
//!
//! A provider is added where a turn's service is put together, and decides for itself whom it
//! answers. For anything of a person's own, the turn's caller passes
//! [`crate::workspaces::Workspaces::asked_turn`] first.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    ActionCallError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueRow,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_server_protocol::WorkspaceId;

use super::OwnAnswers;
use crate::workspaces::Workspaces;

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

/// The app that owns a direct turn provider, including providers whose wire ID names
/// another service or a particular window.
#[derive(Clone)]
pub(super) struct Provider {
    app: &'static str,
    service: Arc<dyn TurnProvider>,
}

/// The direct providers registered for this turn.
#[derive(Clone)]
pub(super) struct Providers {
    entries: Vec<Provider>,
    workspaces: Workspaces,
    workspace: String,
    runtime: tokio::runtime::Handle,
}

impl Providers {
    pub(super) fn here(workspaces: &Workspaces, workspace: WorkspaceId) -> Self {
        Self {
            entries: Vec::new(),
            workspaces: workspaces.clone(),
            workspace: workspace.to_string(),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    pub(super) fn add(&mut self, app: &'static str, service: Arc<dyn TurnProvider>) {
        self.entries.push(Provider { app, service });
    }

    /// List the providers and their actions under the caller's current signed grant.
    /// Separate apps can contribute to a single wire provider, such as Mail and Calendar.
    pub(super) async fn page(
        &self,
        caller: &Caller,
        query: &CatalogueQuery,
        offered: &mut Vec<CatalogueProvider>,
    ) -> Result<Option<CataloguePage>, ActionCallError> {
        let access = self
            .workspaces
            .access_for_agent_caller(&self.workspace, caller)
            .await;
        let mut rows = Vec::new();
        let mut direct = false;
        for entry in &self.entries {
            let allowed = access
                .as_ref()
                .is_some_and(|access| access.permits_app(entry.app));
            if let Some(wanted) = &query.provider {
                if !allowed {
                    // A direct provider that was withdrawn still owns its catalogue route.
                    direct |= entry
                        .service
                        .providers(caller)
                        .iter()
                        .any(|named| named.provider == *wanted);
                } else if let Some(answer) = entry.service.rows(caller, query) {
                    direct = true;
                    if let Some(more) = answer.await {
                        rows.extend(more?);
                    }
                }
            } else if allowed {
                for named in entry.service.providers(caller) {
                    if offered.iter().all(|known| known.provider != named.provider) {
                        offered.push(named);
                    }
                }
            }
        }
        Ok(direct.then(|| CataloguePage::from_rows("0".into(), rows, query)))
    }

    pub(super) fn taking(&self, target: &RemoteActionTarget) -> Option<&Provider> {
        self.entries
            .iter()
            .find(|entry| entry.service.takes(target))
    }

    /// Recheck the owning app immediately before invoking the direct provider. A guessed
    /// address is subject to the same grant as one obtained from the catalogue.
    pub(super) fn carry_out(
        &self,
        provider: &Provider,
        caller: &Caller,
        request: InvokeRequest,
        answered: OwnAnswers,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (completer, answer) = tilo_actions::remote::pending();
        let (workspaces, workspace) = (self.workspaces.clone(), self.workspace.clone());
        let (caller, provider) = (caller.clone(), provider.clone());
        self.runtime.spawn(async move {
            let invocation = request.invocation;
            let untrusted = provider.service.carries_untrusted(&request.target);
            let taken = if workspaces
                .agent_may_act(&workspace, &caller, provider.app)
                .await
            {
                provider.service.invoke(caller.clone(), request).await
            } else {
                Err(ActionCallError::NotPermitted)
            };
            let receipt = match taken {
                Ok(update) => {
                    let state = InvocationState::new(invocation, update).with_untrusted(untrusted);
                    let receipt = state.receipt();
                    answered.keep_state(caller, state);
                    receipt
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
}
