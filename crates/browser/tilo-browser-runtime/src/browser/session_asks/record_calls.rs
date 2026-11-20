//! Actions a window asks of the workspace's server, on a page whose workspace has one: those of
//! a resource of the workspace, and what that server keeps for the workspace's
//! people (its accounts, model providers, vault and what it tells them).
//!
//! Chat asks a shared note for its link; any window may ask a record another app keeps for one of
//! its actions (provider `record:<app>:<record>`). On a native desktop the broker hands that to
//! whatever serves the record. A page reaches the workspace's host, which runs a record's actions
//! whoever has it open and decides each by the asking person's own access: the page grants
//! nothing itself. No device of the person's is asked. The desktop's session carries such an
//! action here
//! ([`tilo_desktop_surface::session::DesktopSession::serve_workspace_api`]).

use std::sync::Arc;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_server_protocol::WorkspaceId;
use tilo_session_actions::SessionActions;

/// The workspace's host as this page reaches it now. The page's session with its host comes and
/// goes, so the host's action API is looked up for every request.
pub(in crate::browser) struct WorkspaceServer {
    peer: u32,
    workspace: WorkspaceId,
}

impl WorkspaceServer {
    /// The host of the workspace `config`'s page shows; `None` for a page with no workspace host.
    pub(in crate::browser) fn of(config: &super::super::Config) -> Option<Arc<Self>> {
        Some(Arc::new(Self {
            peer: config.peer?,
            workspace: crate::peer::workspace_id(&config.workspace).ok()?,
        }))
    }

    fn api(&self) -> Result<SessionActions, ActionCallError> {
        crate::peer_sync::actions_of(self.peer, self.workspace)
            .ok_or_else(crate::peer_sync::no_server)
    }
}

impl ActionProvider for WorkspaceServer {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        match self.api() {
            Ok(api) => api.query(caller, query),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        match self.api() {
            Ok(api) => api.invoke(caller, request),
            Err(error) => Pending::ready(Err(error)),
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.api().ok()?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.api().ok()?.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        match self.api() {
            Ok(api) => api.cancel(caller, id),
            Err(error) => Pending::ready(Err(error)),
        }
    }
}
