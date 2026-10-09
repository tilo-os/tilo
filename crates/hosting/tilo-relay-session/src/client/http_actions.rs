//! Narrow HTTP proof adapter into the existing certified private broker value.
use super::CertifiedAction;
use tilo_delivery_protocol::delivery_v3::{
    auth::Carrier,
    capabilities,
    http_proof::{Method, VerifiedRequest},
    private_wire::{self, PrivateOperation, PrivateScope, WireError},
};
impl CertifiedAction {
    /// Consume the actual admitted request, retaining its certified caller.
    /// `peer` must be borrowed from the native owner's current committed authority
    /// guard. Current broker policy remains mandatory at dequeue/effect/delivery.
    /// The canonical governor is the broker target; the delivery host may differ.
    ///
    /// # Errors
    ///
    /// Returns `WireError` if the input is not a canonical encoding within this type's bounds.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "taking the verified request by value keeps one verification from certifying two actions"
    )]
    pub fn from_http_request(
        request: VerifiedRequest<'_>,
        peer: &tilo_peer_state::StatePeer,
    ) -> Result<Self, WireError> {
        let session = request.session();
        let binding = session.binding();
        let exact = request.request();
        if binding.carrier != Carrier::Https || exact.method() != Method::Post {
            return Err(WireError::Direction);
        }
        let operation = PrivateOperation::from_target("POST", exact.target())?;
        if !capabilities::validate(session.capabilities())
            || session.capabilities() & operation.capability() == 0
        {
            return Err(WireError::Direction);
        }
        let room = uuid::Uuid::parse_str(peer.room()).map_err(|_| WireError::Scope)?;
        if peer.admitted_room().is_none()
            || !crate::room_workspace_id_matches(peer.room(), binding.workspace)
            || peer.authority().user_public_key != binding.workspace_root
        {
            return Err(WireError::Scope);
        }
        let action = private_wire::decode_request(
            operation,
            PrivateScope {
                room,
                source: binding.device,
                target: peer.authority().device_public_key,
            },
            exact.body(),
        )?;
        Ok(Self {
            action,
            certificate: session.device_certificate().clone(),
        })
    }
}
#[cfg(test)]
mod tests;
