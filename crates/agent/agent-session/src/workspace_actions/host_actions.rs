//! The host's whole app action server, split between the agent supervisor and the workspace's
//! page actions.

use tilo_actions::remote::{Caller, Pending, RemoteActionMessage};
use tilo_server_protocol::WorkspaceId;

use super::WorkspaceActions;

/// The host's whole app action server: the agent supervisor's vocabulary
/// (provider `agents`) and the workspace's page actions (everything else).
pub struct HostActions {
    /// Answers `agents` requests.
    pub supervisor: crate::control::SupervisorHost,
    /// Answers requests about records and the workspace.
    pub actions: WorkspaceActions,
}

impl HostActions {
    /// Answer one request `caller` sent to the host for `workspace_id`, or
    /// `None` for a message that is not a request.
    pub fn serve(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        use tilo_server_protocol::agents::AGENTS_PROVIDER;
        let supervisor = match &message {
            RemoteActionMessage::FeedbackRequest { .. }
            | RemoteActionMessage::ContentPutRequest { .. }
            | RemoteActionMessage::ContentGetRequest { .. }
            | RemoteActionMessage::ContentLinkRequest { .. }
            | RemoteActionMessage::SearchRequest { .. }
            | RemoteActionMessage::ChangesRequest { .. }
            | RemoteActionMessage::ChangedRequest { .. }
            | RemoteActionMessage::SurfaceRequest { .. } => false,
            RemoteActionMessage::CatalogueRequest { query, .. } => {
                query.provider.as_deref() == Some(AGENTS_PROVIDER)
            }
            RemoteActionMessage::Invoke(request) => request.target.provider == AGENTS_PROVIDER,
            RemoteActionMessage::Observe { invocation, .. }
            | RemoteActionMessage::Cancel { invocation } => !self.actions.owns(caller, invocation),
            RemoteActionMessage::FeedbackResponse { .. }
            | RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. }
            | RemoteActionMessage::ResourceRead { .. }
            | RemoteActionMessage::ResourceReading { .. }
            | RemoteActionMessage::RetiredResourceRequest
            | RemoteActionMessage::RetiredResourceResponse
            | RemoteActionMessage::ConfirmationRequest { .. }
            | RemoteActionMessage::ConfirmationResponse { .. }
            | RemoteActionMessage::Confirm { .. }
            | RemoteActionMessage::ConfirmResponse { .. }
            | RemoteActionMessage::RetiredResourceWatch { .. }
            | RemoteActionMessage::RetiredResourceReading { .. }
            | RemoteActionMessage::ContentPutResponse { .. }
            | RemoteActionMessage::ContentGetResponse { .. }
            | RemoteActionMessage::ContentLinkResponse { .. }
            | RemoteActionMessage::SearchResponse { .. }
            | RemoteActionMessage::ChangesResponse { .. }
            | RemoteActionMessage::ChangedResponse { .. }
            | RemoteActionMessage::SurfaceResponse { .. }
            | RemoteActionMessage::CatalogueChanged { .. } => return None,
        };
        if supervisor {
            tilo_actions::remote::serve(&self.supervisor, caller, message)
        } else {
            self.actions.serve(workspace_id, caller, message)
        }
    }
}
