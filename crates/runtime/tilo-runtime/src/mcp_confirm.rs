//! The person's confirmation of what an agent asks: put to them on their own
//! device, and the invocation started again with their answer, as they
//! allowed it.
use super::{
    ActionHost, Admission, BridgeError, CONFIRMATION_PATIENCE, InvokeRequest, McpServer,
    RemoteActionTarget, Value,
};

/// The key of an answer naming the arguments the person changed an
/// invocation to before allowing it: what ran instead of the agent's.
pub(super) const EDITED_BY_PERSON: &str = "edited_by_person";

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// Start `request`. An action that asks for confirmation is put to the
    /// person this agent acts for, on their own device, and started again
    /// with their answer; the agent never confirms by its own word.
    ///
    /// Where the person changed the draft before allowing it, `request` is
    /// started again with their arguments, which their confirmation is
    /// bound to, and `edited` holds them.
    pub(super) async fn start_confirmed(
        &self,
        request: &mut InvokeRequest,
        wait: std::time::Duration,
        record: &mut Option<(tilo_actions::InvocationId, &RemoteActionTarget)>,
        edited: &mut Option<serde_json::Map<String, Value>>,
    ) -> Result<Admission, BridgeError> {
        match self.start(request.clone(), wait, record).await {
            Ok(admission) => return Ok(admission),
            Err(BridgeError::Host(tilo_actions::remote::ActionHostError::ConfirmationRequired)) => {
            }
            Err(error) => return Err(error),
        }
        // The person confirms the action as the directory is at this
        // revision.
        if request.revision.is_none()
            && tilo_actions::remote::workspace_provider_room(&request.target.provider).is_none()
        {
            request.revision = Some(self.current_revision(&request.target.provider).await?);
        }
        let asked = request.clone();
        let witnessed = tokio::time::timeout(
            CONFIRMATION_PATIENCE,
            self.at_host(false, move |host, caller| host.confirm(caller, &asked)),
        )
        .await
        .map_err(|_| BridgeError::from("ConfirmationRequired; the person did not answer in time"))?
        .ok_or_else(|| BridgeError::from("Unavailable"))?
        .map_err(BridgeError::Host)?;
        edited.clone_from(&witnessed.arguments);
        witnessed.apply(request);
        self.start(request.clone(), wait, record).await
    }
}
