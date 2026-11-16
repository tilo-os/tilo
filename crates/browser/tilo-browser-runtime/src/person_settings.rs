//! The person's mail accounts and model providers, as their page reaches
//! them.
//!
//! A page cannot open a mail server's session and keeps no vault of its own,
//! so these asks are the workspace host's to answer. The page's desktop
//! hands them to the session it already holds with the host
//! ([`SessionActions`]), as it hands the MCP server there: the host keeps
//! the accounts, proves each sign-in, and seals the secrets in the
//! person's own vault there. Nothing is stored in the page.

use std::sync::Arc;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, person_accounts, person_models,
};
use tilo_desktop_surface::session::DesktopSession;
use tilo_session_client::remote::SessionActions;

/// One person's provider, asked of the host the page's session is with.
struct Forwarded {
    host: SessionActions,
}

impl ActionHost for Forwarded {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.host.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.host.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.host.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.host.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        self.host.cancel(caller, id)
    }
}

/// Carry the accounts and providers asks of the desktop `session` to the
/// host `host` reaches.
pub(crate) fn serve(session: &DesktopSession, host: &SessionActions) {
    for provider in [person_accounts::PROVIDER, person_models::PROVIDER] {
        session.serve_person_door(provider, Arc::new(Forwarded { host: host.clone() }));
    }
}
