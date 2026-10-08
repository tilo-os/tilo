//! An attended desktop chooses inert candidates supplied by their origin host.
use crate::shell::portal_dialog::PortalDialogQueue;
use std::sync::Arc;
use tilo_actions::remote::{ActionHostError, RemoteActionMessage, ResourceSelectionPromptRequest};

pub(crate) fn answer_remote_prompt(
    network: &Arc<super::network::ShellNetwork>,
    inbound: &super::remote_action_link::InboundRemoteAction,
) -> bool {
    let (correlation, prompt) = match &inbound.message {
        RemoteActionMessage::ResourceSelectionPrompt {
            correlation,
            request,
        } => (*correlation, Some(request)),
        RemoteActionMessage::ResourceSelectionPromptCancel { correlation } => (*correlation, None),
        _ => return false,
    };
    let Some(source) = inbound
        .caller
        .device_public_key()
        .copied()
        .filter(|source| source == inbound.from.as_bytes())
    else {
        return true;
    };
    let Some(room) = inbound.caller.room() else {
        return true;
    };
    let queue = PortalDialogQueue::global();
    let Some(prompt) = prompt else {
        queue.withdraw_resource_selection(room, source, correlation);
        network.wake_host_request();
        return true;
    };
    let result = validate_prompt(network, room, prompt).and_then(|()| {
        queue
            .enqueue_resource_selection(source, correlation, prompt.clone())
            .map_err(|reason| ActionHostError::Unavailable { reason })
    });
    if result.is_ok() {
        network.wake_host_request();
    }
    let network = Arc::clone(network);
    let topic = inbound.workspace_topic;
    let peer = inbound.from;
    match result {
        Ok(answer) => answer.then(move |answer| {
            let result = answer.ok_or_else(|| ActionHostError::Unavailable {
                reason: "resource selection retired or disconnected".into(),
            });
            network.send_remote_action(
                topic,
                peer,
                RemoteActionMessage::ResourceSelectionPromptResponse {
                    correlation,
                    result,
                },
            );
        }),
        Err(error) => {
            network.send_remote_action(
                topic,
                peer,
                RemoteActionMessage::ResourceSelectionPromptResponse {
                    correlation,
                    result: Err(error),
                },
            );
        }
    }
    true
}

fn validate_prompt(
    network: &super::network::ShellNetwork,
    room: &str,
    prompt: &ResourceSelectionPromptRequest,
) -> Result<(), ActionHostError> {
    let room_id = super::state::RoomId::parse(room).ok_or(ActionHostError::NotPermitted)?;
    if prompt.room != room
        || network.workspace_of_room(room_id).is_none()
        || network.governed_elsewhere(room_id)
        || !super::app_action_consent::session_attended()
    {
        return Err(ActionHostError::NotPermitted);
    }
    validate_candidates(prompt).map_err(|reason| ActionHostError::InvalidRequest { reason })
}

pub(crate) fn validate_candidates(prompt: &ResourceSelectionPromptRequest) -> Result<(), String> {
    if prompt.room.is_empty()
        || prompt.source_app_id.is_empty()
        || prompt.candidates.is_empty()
        || prompt.candidates.len() > tilo_actions::remote::MAX_RESOURCE_SELECTION_CANDIDATES
    {
        return Err("resource selection metadata exceeds its bounds".into());
    }
    let mut tokens = std::collections::HashSet::new();
    for candidate in &prompt.candidates {
        let selected = &candidate.selected;
        let provider = &selected.provider;
        let row = &selected.row;
        if candidate.token.is_nil()
            || !tokens.insert(candidate.token)
            || provider.room != prompt.room
            || provider.record_id.is_empty()
            || provider.provider != row.provider
            || provider.app_id != row.app_id
            || selected.revision.is_empty()
            || row.resource.id.is_empty()
            || row.action.as_ref().is_none_or(|action| {
                action.resource_kind != row.resource.kind
                    || action
                        .semantic_identity
                        .as_ref()
                        .is_none_or(|identity| !identity.is_valid())
            })
        {
            return Err("resource selection candidate has inconsistent original metadata".into());
        }
    }
    Ok(())
}

pub(super) struct NativeResourceSelections {
    pub(super) owner: Arc<super::broker_services::NativeOwnerCommitService>,
    pub(super) network: Arc<super::network::ShellNetwork>,
}

impl tilo_action_broker::services::ResourceSelectionServices for NativeResourceSelections {
    fn select(
        &self,
        context: tilo_action_broker::services::NativeDocumentReadContext,
        request: ResourceSelectionPromptRequest,
    ) -> tilo_action_broker::services::ResourceSelectionFuture {
        use tilo_action_broker::services::NativeDocumentReads as _;
        let owner = self.owner.clone();
        let network = self.network.clone();
        Box::pin(async move {
            owner.authorize(&context)?;
            context.revalidate()?;
            if request.room != context.route().cluster_id
                || request.source_app_id != context.route().app_id
                || request.source_record_id != context.route().record_id
            {
                return Err("resource selection original source changed".into());
            }
            validate_candidates(&request)?;
            let result = network.choose_original_resource(request).await?;
            context.revalidate()?;
            owner.authorize(&context)?;
            Ok(result)
        })
    }
}
