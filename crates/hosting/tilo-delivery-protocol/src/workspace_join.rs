//! Contract types of the shared workspace join: the signed Iroh host hint, the relay trust
//! it derives, the semantic service callback and its errors.

use serde::{Deserialize, Serialize};
use std::future::Future;
use tilo_peer_state::join::{
    JoinBootstrap, JoinTransportRequest, JoinTransportResponse,
    TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1, TransportHint, sign_transport_hint,
    verify_transport_hint,
};
use tilo_user_identity::DeviceCert;

/// Public Iroh ALPN for invitation redemption and membership refresh.
pub const WORKSPACE_JOIN_ALPN: &[u8] = b"tilo-workspace-join/1";
/// Largest accepted encoded request, including a signed invitation and certificate.
pub const MAX_WORKSPACE_JOIN_REQUEST_BYTES: usize = 256 * 1024;
/// Largest accepted bootstrap response containing governed workspace state.
pub const MAX_WORKSPACE_JOIN_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// Canonical payload for the signed Iroh workspace-host transport hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceHostMode {
    /// Dedicated home/cloud host certified with `can_act_as_host`.
    CertifiedHost,
    /// One-room native owner whose exact governing device terminates service.
    NativeInteractiveWorkspaceOwner,
}

/// Verified relay-authentication scope derived only from signed hint + authority data.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "public trust mode chosen once per join; boxing would change its constructors in dependent crates"
)]
pub enum WorkspaceHostTrust {
    /// Existing dedicated-host authentication remains capability-gated.
    CertifiedHost,
    /// Narrow exception for the exact interactive room governor.
    InteractiveOwner(InteractiveOwnerTrust),
}

/// Opaque proof that a primary device may host one exact governed workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractiveOwnerTrust {
    room: String,
    workspace_id: u64,
    authority: DeviceCert,
    service_public_key: [u8; 32],
}

/// Bootstrap paired with the verified relay-authentication scope that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceJoinAdmission {
    /// Authoritative governed state committed before the host replied.
    pub bootstrap: JoinBootstrap,
    /// Trust mode and exact scope required for the subsequent relay connection.
    pub trust: WorkspaceHostTrust,
}

impl InteractiveOwnerTrust {
    /// Canonical room UUID this exception covers.
    pub fn room(&self) -> &str {
        &self.room
    }

    /// Exact workspace ID this exception covers.
    pub fn workspace_id(&self) -> u64 {
        self.workspace_id
    }

    /// Exact governing certificate that must answer relay authentication.
    pub fn authority(&self) -> &DeviceCert {
        &self.authority
    }

    /// Exact Iroh service key terminating the signed endpoint.
    pub fn service_public_key(&self) -> [u8; 32] {
        self.service_public_key
    }

    /// Verify that `authority` may govern exactly `room`/`workspace_id` right now, and bind
    /// that scope to the Iroh service key terminating it.
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the room/workspace binding or the certificate is invalid.
    pub fn verify(
        room: String,
        workspace_id: u64,
        authority: DeviceCert,
        service_public_key: [u8; 32],
    ) -> Result<Self, WorkspaceJoinError> {
        let canonical_room = uuid::Uuid::parse_str(&room)
            .map(|room| room.to_string())
            .map_err(|_| {
                WorkspaceJoinError::Protocol("interactive owner room is invalid".into())
            })?;
        if canonical_room != room
            || !tilo_relay_protocol::room_workspace_id_matches(&room, workspace_id)
        {
            return Err(WorkspaceJoinError::Protocol(
                "interactive owner room/workspace binding is invalid".into(),
            ));
        }
        tilo_user_identity::verify_device_cert_at(&authority, tilo_clock::unix_now_ns())
            .map_err(|error| WorkspaceJoinError::Protocol(error.to_string()))?;
        if !authority.capabilities.can_own_workspaces {
            return Err(WorkspaceJoinError::Protocol(
                "interactive owner certificate cannot govern workspaces".into(),
            ));
        }
        Ok(InteractiveOwnerTrust {
            room,
            workspace_id,
            authority,
            service_public_key,
        })
    }
}

/// Canonical payload for the signed Iroh workspace-host transport hint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrohWorkspaceHostHintV1 {
    /// Frozen payload version.
    pub version: u32,
    /// Explicit trust mode; never inferred from an unsigned local setting.
    pub mode: WorkspaceHostMode,
    /// Canonical governed room UUID served by this endpoint.
    pub room: String,
    /// Exact governed workspace ID served by this endpoint.
    pub workspace_id: u64,
    /// Pinned governing device for the room.
    pub governing_authority: DeviceCert,
    /// Exact Iroh service address the guest dials.
    pub endpoint_addr: iroh_base::EndpointAddr,
    /// Person root that certified the host device.
    pub host_user_public_key: [u8; 32],
    /// Root-certified host identity that signed the service key binding.
    pub host_device_certificate: DeviceCert,
    /// HTTP packet relay used by this private/local Iroh deployment.
    pub packet_relay_url: String,
}

impl IrohWorkspaceHostHintV1 {
    /// Sign this opaque address payload and its distinct Iroh service key with the host device.
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the inputs violate this operation's bounds or invariants.
    pub fn transport_hint(
        &self,
        host_key: &ed25519_dalek::SigningKey,
    ) -> Result<TransportHint, WorkspaceJoinError> {
        self.validate()?;
        sign_transport_hint(
            TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1,
            serde_json::to_string(self)
                .map_err(|error| WorkspaceJoinError::Encode(error.to_string()))?,
            self.host_device_certificate.clone(),
            *self.endpoint_addr.id.as_bytes(),
            host_key,
        )
        .map_err(WorkspaceJoinError::Protocol)
    }

    /// Parse and verify a signed Iroh workspace-host hint.
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the inputs violate this operation's bounds or invariants.
    pub fn from_transport_hint(hint: &TransportHint) -> Result<Self, WorkspaceJoinError> {
        if hint.kind != TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1 {
            return Err(WorkspaceJoinError::Protocol(
                "transport hint is not an Iroh workspace host".into(),
            ));
        }
        let value: Self = serde_json::from_str(&hint.opaque_json)
            .map_err(|error| WorkspaceJoinError::Decode(error.to_string()))?;
        if hint.host != value.host_device_certificate {
            return Err(WorkspaceJoinError::Protocol(
                "workspace-host hint payload does not match its signed host pin".into(),
            ));
        }
        verify_transport_hint(hint).map_err(WorkspaceJoinError::Protocol)?;
        if hint.service_public_key != *value.endpoint_addr.id.as_bytes() {
            return Err(WorkspaceJoinError::Protocol(
                "workspace-host service pin does not match its Iroh endpoint".into(),
            ));
        }
        value.validate()?;
        Ok(value)
    }

    /// Verify a signed hint and derive its opaque relay trust without contacting the host.
    ///
    /// This is the restart-safe path for an already persisted `TransportHint`. The trust token
    /// cannot be derived from the public payload struct alone because this method first verifies
    /// the host signature and its exact service-key binding.
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the value fails validation or verification.
    pub fn authenticate_transport_hint(
        hint: &TransportHint,
    ) -> Result<(Self, WorkspaceHostTrust), WorkspaceJoinError> {
        let host = Self::from_transport_hint(hint)?;
        let trust = host.verified_trust();
        Ok((host, trust))
    }

    /// Require an inspected invitation to name this exact governed host scope.
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the value fails validation or verification.
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "the grant is checked against this host's governing authority, not a field of the grant itself"
    )]
    pub fn validate_invitation(
        &self,
        grant: &tilo_peer_state::join::InviteGrant,
    ) -> Result<WorkspaceHostTrust, WorkspaceJoinError> {
        if grant.room != self.room
            || grant.workspace_id != self.workspace_id
            || grant.authority != self.governing_authority
        {
            return Err(WorkspaceJoinError::Protocol(
                "invitation scope does not match its signed workspace host hint".into(),
            ));
        }
        match self.mode {
            WorkspaceHostMode::CertifiedHost if grant.issuer == self.host_device_certificate => {}
            WorkspaceHostMode::CertifiedHost => {
                return Err(WorkspaceJoinError::Protocol(
                    "certified workspace host did not issue the invitation".into(),
                ));
            }
            WorkspaceHostMode::NativeInteractiveWorkspaceOwner
                if grant.issuer == self.governing_authority && grant.delegation.is_none() => {}
            WorkspaceHostMode::NativeInteractiveWorkspaceOwner => {
                return Err(WorkspaceJoinError::Protocol(
                    "interactive owner invitation is not signed by its exact governor".into(),
                ));
            }
        }
        Ok(self.verified_trust())
    }

    /// Require a returned bootstrap to preserve the signed host/governor scope.
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the value fails validation or verification.
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "the grant is checked against this host's governing authority, not a field of the grant itself"
    )]
    pub fn validate_bootstrap(
        &self,
        bootstrap: &JoinBootstrap,
    ) -> Result<WorkspaceHostTrust, WorkspaceJoinError> {
        if bootstrap.room != self.room
            || bootstrap.workspace_id != self.workspace_id
            || bootstrap.authority != self.governing_authority
        {
            return Err(WorkspaceJoinError::Protocol(
                "workspace bootstrap does not match its signed host hint".into(),
            ));
        }
        Ok(self.verified_trust())
    }

    fn verified_trust(&self) -> WorkspaceHostTrust {
        match self.mode {
            WorkspaceHostMode::CertifiedHost => WorkspaceHostTrust::CertifiedHost,
            WorkspaceHostMode::NativeInteractiveWorkspaceOwner => {
                WorkspaceHostTrust::InteractiveOwner(InteractiveOwnerTrust {
                    room: self.room.clone(),
                    workspace_id: self.workspace_id,
                    authority: self.governing_authority.clone(),
                    service_public_key: *self.endpoint_addr.id.as_bytes(),
                })
            }
        }
    }

    fn validate(&self) -> Result<(), WorkspaceJoinError> {
        let canonical_room = uuid::Uuid::parse_str(&self.room)
            .map(|room| room.to_string())
            .map_err(|_| WorkspaceJoinError::Protocol("workspace host room is invalid".into()))?;
        if self.version != 1
            || canonical_room != self.room
            || !tilo_relay_protocol::room_workspace_id_matches(&self.room, self.workspace_id)
            || self.host_device_certificate.user_public_key != self.host_user_public_key
        {
            return Err(WorkspaceJoinError::Protocol(
                "workspace-host transport hint identity is inconsistent".into(),
            ));
        }
        let now_ns = tilo_clock::unix_now_ns();
        tilo_user_identity::verify_device_cert_at(&self.host_device_certificate, now_ns)
            .map_err(|error| WorkspaceJoinError::Protocol(error.to_string()))?;
        tilo_user_identity::verify_device_cert_at(&self.governing_authority, now_ns)
            .map_err(|error| WorkspaceJoinError::Protocol(error.to_string()))?;
        match self.mode {
            WorkspaceHostMode::CertifiedHost
                if !self.host_device_certificate.capabilities.can_act_as_host =>
            {
                return Err(WorkspaceJoinError::Protocol(
                    "certified workspace host lacks host capability".into(),
                ));
            }
            WorkspaceHostMode::NativeInteractiveWorkspaceOwner
                if self.host_device_certificate != self.governing_authority
                    || !self.governing_authority.capabilities.can_own_workspaces =>
            {
                return Err(WorkspaceJoinError::Protocol(
                    "interactive workspace host is not the exact room governor".into(),
                ));
            }
            _ => {}
        }
        if self.packet_relay_url.is_empty() || self.packet_relay_url.len() > 4096 {
            return Err(WorkspaceJoinError::Protocol(
                "workspace-host packet relay URL is invalid".into(),
            ));
        }
        Ok(())
    }
}

/// Serialized semantic callback implemented by each workspace authority store.
pub trait WorkspaceJoinService: Clone + Send + Sync + std::fmt::Debug + 'static {
    /// Handle one typed request bound to the authenticated QUIC endpoint key.
    fn handle(
        &self,
        remote_endpoint: [u8; 32],
        request: JoinTransportRequest,
    ) -> impl Future<Output = JoinTransportResponse> + Send;
}

/// Failures before or returned by the semantic workspace authority.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceJoinError {
    /// Iroh or stream failure.
    #[error("workspace join transport failed: {0}")]
    Transport(String),
    /// The bounded exchange deadline elapsed.
    #[error("workspace join request timed out")]
    Timeout,
    /// Local request serialization failed.
    #[error("workspace join request encoding failed: {0}")]
    Encode(String),
    /// Remote response decoding failed.
    #[error("workspace join response decoding failed: {0}")]
    Decode(String),
    /// The typed request or response violated the protocol contract.
    #[error("workspace join protocol rejected the exchange: {0}")]
    Protocol(String),
    /// The authoritative service rejected a well-formed request.
    #[error("workspace join host rejected the request ({code}): {message}")]
    Remote {
        /// Stable HTTP-like status code used across transports.
        code: u16,
        /// Public-safe rejection reason.
        message: String,
    },
}

mod delivery_origin;
pub use delivery_origin::{DeliveryOriginLocatorV1, TRANSPORT_DELIVERY_ORIGIN_V1};
