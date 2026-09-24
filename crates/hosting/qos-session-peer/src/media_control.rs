//! Exact command-to-route binding; request authority remains the single HTTP registry.
use crate::{
    SessionOwner, https::MediaRoute, media_authority::MediaError, media_service::MediaService,
};
use tilo_relay::delivery_v3::{
    http_proof::VerifiedRequest,
    media_wire::{ControlCommand, ControlReply},
};
pub(crate) fn route(command: &ControlCommand) -> MediaRoute {
    match command {
        ControlCommand::GrantChallenge { .. } => MediaRoute::GrantChallenge,
        ControlCommand::GrantComplete { .. } => MediaRoute::GrantComplete,
        ControlCommand::RelayChallenge { .. } => MediaRoute::RelayChallenge,
        ControlCommand::RefreshChallenge { .. } => MediaRoute::RefreshChallenge,
        ControlCommand::RefreshComplete { .. } => MediaRoute::RefreshComplete,
        ControlCommand::RelayClose { .. } => MediaRoute::RelayClose,
        ControlCommand::Presence { .. } => MediaRoute::Presence,
    }
}
pub(crate) fn dispatch(
    owner: &SessionOwner,
    service: &MediaService,
    request: &VerifiedRequest<'_>,
) -> Result<Vec<u8>, MediaError> {
    ControlReply {
        version: 1,
        result: service.control(owner, request).inspect_err(|error| {
            // Content-free: route and error class only.
            tracing::debug!(target = request.request().target(), reason = ?error, "media control rejected");
        })?,
    }
    .encode()
    .map_err(|_| MediaError::InvalidRequest)
}
