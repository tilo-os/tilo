//! What the agents' turns and their replies act through, wherever the engine runs.
//!
//! On a workspace's host that is the host's own action service, which answers the members of
//! the workspaces it governs. On a person's own device it is the device's broker: the same
//! provider the device's apps and the person's other devices call, which decides each call as
//! it decides theirs. The engine asks both the same questions.

use std::sync::Arc;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvocationState, Pending,
    ReceiptStatus, RemoteActionMessage, ResourceReads,
};
use tilo_agent_session::workspace_actions::WorkspaceActions;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;

/// Where a turn's calls go.
#[derive(Clone)]
pub enum EngineActions {
    /// The host's own action service, over the workspaces it governs.
    Host(WorkspaceActions),
    /// A person's own device's broker, over the workspaces the device holds, and the keys the
    /// device's agents sign their calls with there.
    Device(
        Arc<dyn ActionProvider + Send + Sync>,
        Arc<dyn super::agent_keys::AgentKeys>,
    ),
}

impl From<WorkspaceActions> for EngineActions {
    fn from(actions: WorkspaceActions) -> Self {
        Self::Host(actions)
    }
}

impl EngineActions {
    /// The host's action service, where the engine runs on a host: what only a host serves (the
    /// questions a turn asks in its conversation, the events and initiative it keeps) is reached
    /// through it.
    #[must_use]
    pub fn host(&self) -> Option<&WorkspaceActions> {
        match self {
            Self::Host(actions) => Some(actions),
            Self::Device(..) => None,
        }
    }

    /// Answer `message`, a request `caller` makes in `workspace`, or `None` for a message that
    /// is not a request these actions answer.
    pub(super) fn serve(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        match self {
            Self::Host(actions) => actions.serve(workspace, caller, message),
            Self::Device(provider, _) => serve_device(provider.as_ref(), caller, message),
        }
    }

    /// `request`, which its person allowed when the turn asked them in its conversation, made
    /// for `caller`. A host confirms that one invocation itself; a device's broker decides it as
    /// any other call of the person's own, where what still asks is put to them again.
    pub(super) fn serve_allowed(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> Option<Pending<RemoteActionMessage>> {
        match self {
            Self::Host(actions) => actions.serve_allowed(workspace, caller, request),
            Self::Device(provider, _) => serve_device(
                provider.as_ref(),
                caller,
                RemoteActionMessage::Invoke(request),
            ),
        }
    }

    /// Sign `request` as the agent `caller` is, with that agent's own key: on a host the key of
    /// its approved session, on the person's device the key that device made for it. A person's
    /// own call is left as it is.
    ///
    /// # Errors
    /// `caller` is an agent whose key is not held here.
    pub(super) fn sign_for(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        request: &mut tilo_actions::remote::InvokeRequest,
    ) -> Result<(), String> {
        match self {
            Self::Host(actions) => actions.sign_for(workspace, caller, request),
            Self::Device(_, keys) => {
                if caller.roster_created_at().is_none() {
                    return Ok(());
                }
                let key = caller
                    .device_public_key()
                    .ok_or("the agent names no key it signs with")?;
                keys.sign(workspace, key, request)
            }
        }
    }

    /// How invocation `id` of `caller` stands, where these actions hold it.
    pub(super) fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        match self {
            Self::Host(actions) => actions.observe(caller, id),
            Self::Device(provider, _) => provider.observe(caller, id),
        }
    }

    /// Another's write landed in `route` of `workspace`: the host's service acts on it at its
    /// next request. A device's broker reads its store as it is, and is told nothing.
    pub(super) fn landed(&self, workspace: WorkspaceId, route: DocumentRoute) {
        if let Self::Host(actions) = self {
            actions.landed(workspace, route);
        }
    }

    /// One resource of the workspace, read as every reader of it is answered.
    pub(super) fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
        match self {
            Self::Host(actions) => Some(Arc::new(actions.clone())),
            Self::Device(provider, _) => provider.resource_reads(),
        }
    }
}

/// `message` answered by a device's broker, through the provider calls each request is.
fn serve_device(
    provider: &(dyn ActionProvider + Send + Sync),
    caller: &Caller,
    message: RemoteActionMessage,
) -> Option<Pending<RemoteActionMessage>> {
    let unserved = || ActionCallError::Unavailable {
        reason: "this device's broker does not serve that request".into(),
    };
    Some(match message {
        RemoteActionMessage::CatalogueRequest { correlation, query } => provider
            .query(caller, query)
            .map(move |result| RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            }),
        RemoteActionMessage::Invoke(request) => {
            let invocation = request.invocation;
            provider
                .invoke(caller, request)
                .map(move |answer| match answer {
                    Ok(receipt) => RemoteActionMessage::Receipt(receipt),
                    Err(error) => RemoteActionMessage::Receipt(InvocationReceipt {
                        invocation,
                        status: ReceiptStatus::Rejected { error },
                    }),
                })
        }
        RemoteActionMessage::Cancel { invocation } => provider
            .cancel(caller, &invocation)
            .map(move |result| RemoteActionMessage::CancelResponse { invocation, result }),
        RemoteActionMessage::SearchRequest {
            correlation,
            needle,
            within,
        } => {
            let respond = move |result| RemoteActionMessage::SearchResponse {
                correlation,
                result,
            };
            match provider.resource_search() {
                Some(search) => search.find_within(caller, &needle, &within).map(respond),
                None => Pending::ready(respond(Err(unserved()))),
            }
        }
        RemoteActionMessage::ChangesRequest { correlation, after } => {
            let respond = move |result| RemoteActionMessage::ChangesResponse {
                correlation,
                result,
            };
            match provider.change_feed() {
                Some(feed) => feed.since(caller, after.as_deref()).map(respond),
                None => Pending::ready(respond(Err(unserved()))),
            }
        }
        RemoteActionMessage::ChangedRequest { correlation, scope } => provider
            .changed(caller, scope)?
            .map(move |result| RemoteActionMessage::ChangedResponse {
                correlation,
                result,
            }),
        _ => return None,
    })
}
