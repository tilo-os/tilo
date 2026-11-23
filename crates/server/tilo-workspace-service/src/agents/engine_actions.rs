//! What the agents' turns and their replies act through, wherever the engine runs: the
//! device's dispatch, the same provider the device's apps and the person's other devices call,
//! which decides each call as it decides theirs.

use std::sync::Arc;

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvocationState, Pending,
    ReceiptStatus, RemoteActionMessage, ResourceReads,
};
use tilo_server_protocol::WorkspaceId;

/// Where a turn's calls go: the device's dispatch over the workspaces on the device, and the
/// keys the device's agents sign their calls with there.
#[derive(Clone)]
pub struct EngineActions {
    dispatch: Arc<dyn ActionProvider + Send + Sync>,
    keys: Arc<dyn super::agent_keys::AgentKeys>,
}

impl EngineActions {
    /// Calls made through `dispatch`, an agent's signed with its key from `keys`.
    #[must_use]
    pub fn new(
        dispatch: Arc<dyn ActionProvider + Send + Sync>,
        keys: Arc<dyn super::agent_keys::AgentKeys>,
    ) -> Self {
        Self { dispatch, keys }
    }

    /// Answer `message`, a request `caller` makes, or `None` for a message that is not a
    /// request these actions answer.
    pub(super) fn serve(
        &self,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        serve_device(self.dispatch.as_ref(), caller, message)
    }

    /// Sign `request` as the agent `caller` is, with the key this device made for it. A
    /// person's own call is left as it is.
    ///
    /// # Errors
    /// `caller` is an agent that names no key, or that the workspace's list of its agents does
    /// not name, or whose key is not held here: a call made in an agent's name is never sent
    /// without that agent's signature.
    pub(super) fn sign_for(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        request: &mut tilo_actions::remote::InvokeRequest,
    ) -> Result<(), String> {
        // A person's own call is the device's, and so is a session the device hands to
        // software: only an agent acting with its own key is signed by that key or not sent
        // at all.
        if !caller.acting().is_agent() || caller.is_session_delegated() {
            return Ok(());
        }
        let key = caller
            .device_public_key()
            .ok_or("the agent names no key it signs with")?;
        if caller.roster_created_at().is_none() {
            return Err("this agent isn't in the workspace's roster".into());
        }
        self.keys.sign(workspace, key, request)
    }

    /// How invocation `id` of `caller` stands, where the dispatch keeps it.
    pub(super) fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.dispatch.observe(caller, id)
    }

    /// One resource of the workspace, read as every reader of it is answered.
    pub(super) fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
        self.dispatch.resource_reads()
    }
}

/// `message` answered by the device's dispatch, through the provider calls each request is.
fn serve_device(
    provider: &(dyn ActionProvider + Send + Sync),
    caller: &Caller,
    message: RemoteActionMessage,
) -> Option<Pending<RemoteActionMessage>> {
    let unserved = || ActionCallError::Unavailable {
        reason: "this device's dispatch does not serve that request".into(),
    };
    Some(match message {
        RemoteActionMessage::CatalogueRequest { correlation, query } => provider
            .query(caller, query)
            .map(move |result| RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            }),
        RemoteActionMessage::Invoke(request)
            if tilo_actions::remote::search::is_find(&request.target) =>
        {
            tilo_actions::remote::search::searched(provider, caller, &request)
                .map(RemoteActionMessage::Receipt)
        }
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
