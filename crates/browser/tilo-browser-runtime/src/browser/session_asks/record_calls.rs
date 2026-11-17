//! Actions a window asks of a record of the workspace, on a page whose workspace has a host.
//!
//! Chat asks a shared note for its link; any window may ask a record another app keeps for one of
//! its actions (provider `record:<app>:<record>`). On a native desktop the broker hands that to
//! whatever serves the record. A page reaches the workspace's host, which runs a record's actions
//! whoever has it open and decides each by the asking person's own access: the page grants
//! nothing itself. The desktop's session carries such an action here
//! ([`tilo_desktop_surface::session::DesktopSession::serve_workspace_api`]).

use std::sync::Arc;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_server_protocol::WorkspaceId;
use tilo_session_client::remote::SessionActions;

/// The workspace's host as this page reaches it now. The page's session with its host comes and
/// goes, so the host's action API is looked up for every request.
pub(in crate::browser) struct WorkspaceHost {
    peer: u32,
    workspace: WorkspaceId,
}

impl WorkspaceHost {
    /// The host of the workspace `config`'s page shows; `None` for a page with no workspace host.
    pub(in crate::browser) fn of(config: &super::super::Config) -> Option<Arc<Self>> {
        Some(Arc::new(Self {
            peer: config.peer?,
            workspace: crate::peer::workspace_id(&config.workspace).ok()?,
        }))
    }

    fn api(&self) -> Result<SessionActions, ActionCallError> {
        crate::peer_sync::actions_of(self.peer, self.workspace).ok_or_else(|| {
            ActionCallError::Unavailable {
                reason: "The workspace's host can't be reached from here right now".into(),
            }
        })
    }
}

impl ActionProvider for WorkspaceHost {
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
