//! The person's agent door, as their page carries it
//! ([`tilo_actions::remote::agent_door`]).
//!
//! The door is served by the person's host: what it let in, read live, and
//! stopping one run are asked there, through the page's session with it
//! ([`SessionActions`]). Taking a grant back is the person's root's to
//! sign, and the root is in this tab, not on the host: a revoke is handed
//! to the page ([`crate::shell::HostRequest::Revoke`]), which signs it and
//! announces it on every session of the person's, as it revokes any of
//! their devices. The door then leaves the agent out, and its read says so.
//! Where agents connect over MCP is the page's to say, when the host does
//! not.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_actions::remote::agent_door::{self, GrantRef};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_desktop_surface::session::DesktopSession;
use tilo_lock::MutexRecover as _;
use tilo_session_core::remote::SessionActions;

/// The keys the person took back here, for the page's root to sign.
pub(crate) type Revoked = Arc<Mutex<Vec<Revoking>>>;

pub(crate) use crate::shell::Revoking;

/// The agent door as the page's desktop asks it.
struct PageDoor {
    host: SessionActions,
    /// Where agents connect over MCP, as the page names it.
    endpoint: Option<String>,
    revoked: Revoked,
    /// Raised with each revoke, so the desktop takes it on its next frame.
    woken: Arc<AtomicBool>,
}

impl PageDoor {
    fn new(
        host: SessionActions,
        endpoint: Option<String>,
        revoked: Revoked,
        woken: Arc<AtomicBool>,
    ) -> Self {
        Self {
            host,
            endpoint: endpoint.filter(|endpoint| !endpoint.is_empty()),
            revoked,
            woken,
        }
    }

    /// Hand the page the grant `request` takes back.
    fn revoke(&self, request: &InvokeRequest) -> Result<InvocationReceipt, ActionHostError> {
        let InvokeArgument::Json(argument) = &request.argument else {
            return Err(invalid("a revoke names its grant"));
        };
        let grant: GrantRef = serde_json::from_value(Value::Object(argument.clone()))
            .map_err(|error| invalid(&error.to_string()))?;
        if hex::decode(&grant.grant).map_or(true, |key| key.len() != 32) {
            return Err(invalid("a grant is named by its device's key"));
        }
        self.revoked.lock_recover().push(Revoking {
            key: grant.grant,
            reason: "retired".to_owned(),
        });
        self.woken.store(true, Ordering::Release);
        Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput::default(),
            },
        })
    }

    /// `state` with the page's MCP address, where the door's read leaves it
    /// out.
    fn with_endpoint(&self, mut state: InvocationState) -> InvocationState {
        if let (Some(endpoint), InvocationUpdate::Item { value, .. }) =
            (&self.endpoint, &mut state.update)
            && let Some(door) = value.as_object_mut()
            && door
                .get("endpoint")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            door.insert("endpoint".into(), Value::String(endpoint.clone()));
        }
        state
    }
}

/// Carry the agent-door asks of the desktop `session` serves, for the
/// workspace `workspace`, to the host the page's peer `peer` is with, and the
/// asks of the person's devices there too ([`crate::identity_door`]); the
/// keys the person takes back at either, for the page to sign. A page with no
/// peer has no door: its asks say none answers.
pub(crate) fn serve(
    session: &DesktopSession,
    peer: Option<u32>,
    workspace: &str,
    endpoint: Option<String>,
    woken: &Arc<AtomicBool>,
) -> Revoked {
    let revoked = Revoked::default();
    if let (Some(peer), Ok(workspace)) = (peer, crate::peer::workspace_id(workspace))
        && let Some(door) = crate::peer_sync::actions_of(peer, workspace)
    {
        session.serve_person_door(
            agent_door::PROVIDER,
            Arc::new(PageDoor::new(
                door.clone(),
                endpoint,
                Arc::clone(&revoked),
                Arc::clone(woken),
            )),
        );
        if let Some(own) = crate::peer_sync::device_of(peer) {
            crate::identity_door::serve(session, door, own, &revoked, woken);
        }
    }
    revoked
}

fn invalid(reason: &str) -> ActionHostError {
    ActionHostError::InvalidRequest {
        reason: reason.to_owned(),
    }
}

impl ActionHost for PageDoor {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Ok(CataloguePage {
            exhausted: true,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if request.target == agent_door::target(agent_door::REVOKE) {
            return Pending::ready(self.revoke(&request));
        }
        self.host.invoke(caller, request)
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.host
            .observe(caller, id)
            .map(|state| self.with_endpoint(state))
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.host.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        self.host.cancel(caller, id)
    }
}
