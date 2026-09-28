//! Inert invitation discovery locator; signatures do not grant session authority.
use super::WorkspaceJoinError;
use tilo_peer_state::join::TransportHint;
use tilo_user_identity::DeviceCert;

/// Kind bound by the existing `TransportHint` signing domain.
pub const TRANSPORT_DELIVERY_ORIGIN_V1: &str = "tilo-delivery-origin-v1";
/// An independently pinned origin locator, never descriptor/session authority.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryOriginLocatorV1 {
    /// Version exactly one.
    pub version: u8,
    /// Exact HTTPS origin; never normalized or redirected.
    pub origin: String,
}
impl DeliveryOriginLocatorV1 {
    fn validate(&self) -> Result<(), WorkspaceJoinError> {
        if self.version != 1 || self.origin.len() > 512 {
            return Err(WorkspaceJoinError::Protocol(
                "invalid delivery origin locator".into(),
            ));
        }
        crate::delivery_v3::descriptor::validate_origin(&self.origin)
            .map_err(|_| WorkspaceJoinError::Protocol("invalid delivery origin".into()))
    }
    /// Sign inert discovery bytes with the named host key. Installation is the
    /// caller's private native responsibility, not a property of this utility.
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the inputs violate this operation's bounds or invariants.
    pub fn transport_hint(
        &self,
        host: &DeviceCert,
        key: &ed25519_dalek::SigningKey,
    ) -> Result<TransportHint, WorkspaceJoinError> {
        self.validate()?;
        let payload = serde_json::to_string(self)
            .map_err(|error| WorkspaceJoinError::Encode(error.to_string()))?;
        tilo_peer_state::join::sign_transport_hint(
            TRANSPORT_DELIVERY_ORIGIN_V1,
            payload,
            host.clone(),
            host.device_public_key,
            key,
        )
        .map_err(WorkspaceJoinError::Protocol)
    }
    /// Verify exact pinned certificate and signed discovery bytes. The caller
    /// must derive the pin from independently/currently verified invitation
    /// state; this does not perform current roster or revocation authorization.
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the inputs violate this operation's bounds or invariants.
    pub fn from_transport_hint(
        hint: &TransportHint,
        expected_host: &DeviceCert,
    ) -> Result<Self, WorkspaceJoinError> {
        if hint.kind != TRANSPORT_DELIVERY_ORIGIN_V1
            || hint.host != *expected_host
            || hint.service_public_key != expected_host.device_public_key
            || hint.opaque_json.len() > 1024
        {
            return Err(WorkspaceJoinError::Protocol(
                "delivery locator pin or bound mismatch".into(),
            ));
        }
        let locator: Self = serde_json::from_str(&hint.opaque_json)
            .map_err(|error| WorkspaceJoinError::Decode(error.to_string()))?;
        locator.validate()?;
        if serde_json::to_string(&locator)
            .map_err(|error| WorkspaceJoinError::Encode(error.to_string()))?
            != hint.opaque_json
        {
            return Err(WorkspaceJoinError::Protocol(
                "noncanonical delivery locator".into(),
            ));
        }
        tilo_peer_state::join::verify_transport_hint(hint).map_err(WorkspaceJoinError::Protocol)?;
        Ok(locator)
    }
}
#[cfg(test)]
mod tests;
