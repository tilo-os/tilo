//! Owner-neutral Iroh transport for the shared workspace join contract.

#[cfg(not(target_arch = "wasm32"))]
use iroh::protocol::{AcceptError, ProtocolHandler};
use qos_peer_state::join::{
    JoinBootstrap, JoinChallengeRequest, JoinTransportRequest, JoinTransportResponse,
    MembershipChallengeRequest, MembershipCompleteRequest,
    TRANSPORT_IROH_WORKSPACE_HOST_ENDPOINT_ADDR_V1, TransportHint, sign_join_proof,
    sign_membership_proof, sign_transport_hint, verify_transport_hint,
};
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
use std::{future::Future, time::Duration};
use tilo_user_identity::DeviceCert;

/// Public Iroh ALPN for invitation redemption and membership refresh.
pub const WORKSPACE_JOIN_ALPN: &[u8] = b"tilo-workspace-join/1";
/// Largest accepted encoded request, including a signed invitation and certificate.
pub const MAX_WORKSPACE_JOIN_REQUEST_BYTES: usize = 256 * 1024;
/// Largest accepted bootstrap response containing governed workspace state.
pub const MAX_WORKSPACE_JOIN_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(target_arch = "wasm32"))]
const MAX_CONCURRENT_EXCHANGES: usize = 64;

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
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn interactive_owner_trust(
    room: String,
    workspace_id: u64,
    authority: DeviceCert,
    service_public_key: [u8; 32],
) -> Result<InteractiveOwnerTrust, WorkspaceJoinError> {
    let canonical_room = uuid::Uuid::parse_str(&room)
        .map(|room| room.to_string())
        .map_err(|_| WorkspaceJoinError::Protocol("interactive owner room is invalid".into()))?;
    if canonical_room != room || !crate::room_workspace_id_matches(&room, workspace_id) {
        return Err(WorkspaceJoinError::Protocol(
            "interactive owner room/workspace binding is invalid".into(),
        ));
    }
    tilo_user_identity::verify_device_cert_at(&authority, crate::clock::now_ns())
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
    pub endpoint_addr: iroh::EndpointAddr,
    /// Person root that certified the host device.
    pub host_user_public_key: [u8; 32],
    /// Root-certified host identity that signed the service key binding.
    pub host_device_certificate: DeviceCert,
    /// HTTP packet relay used by this private/local Iroh deployment.
    pub packet_relay_url: String,
}

impl IrohWorkspaceHostHintV1 {
    /// Sign this opaque address payload and its distinct Iroh service key with the host device.
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
    pub fn authenticate_transport_hint(
        hint: &TransportHint,
    ) -> Result<(Self, WorkspaceHostTrust), WorkspaceJoinError> {
        let host = Self::from_transport_hint(hint)?;
        let trust = host.verified_trust();
        Ok((host, trust))
    }

    /// Require an inspected invitation to name this exact governed host scope.
    pub fn validate_invitation(
        &self,
        grant: &qos_peer_state::join::InviteGrant,
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
            || !crate::room_workspace_id_matches(&self.room, self.workspace_id)
            || self.host_device_certificate.user_public_key != self.host_user_public_key
        {
            return Err(WorkspaceJoinError::Protocol(
                "workspace-host transport hint identity is inconsistent".into(),
            ));
        }
        let now_ns = crate::clock::now_ns();
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

/// Iroh protocol handler that provides bounded postcard request/response framing.
#[derive(Clone, Debug)]
#[cfg(not(target_arch = "wasm32"))]
pub struct WorkspaceJoinProtocol<S> {
    service: S,
    exchanges: Arc<tokio::sync::Semaphore>,
}

#[cfg(not(target_arch = "wasm32"))]
impl<S> WorkspaceJoinProtocol<S> {
    /// Wrap a semantic service without taking ownership of its persistence model.
    #[must_use]
    pub fn new(service: S) -> Self {
        Self {
            service,
            exchanges: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_EXCHANGES)),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<S: WorkspaceJoinService> ProtocolHandler for WorkspaceJoinProtocol<S> {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
        let _permit = self.exchanges.clone().try_acquire_owned().map_err(|_| {
            AcceptError::from_err(std::io::Error::other(
                "workspace join exchange concurrency limit reached",
            ))
        })?;
        n0_future::time::timeout(EXCHANGE_TIMEOUT, self.exchange(&connection))
            .await
            .map_err(|_| {
                AcceptError::from_err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "workspace join exchange timed out",
                ))
            })?
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<S: WorkspaceJoinService> WorkspaceJoinProtocol<S> {
    async fn exchange(
        &self,
        connection: &iroh::endpoint::Connection,
    ) -> Result<(), WorkspaceJoinError> {
        let remote_endpoint = *connection.remote_id().as_bytes();
        let (mut send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        let bytes = crate::framing::read_frame(&mut recv, MAX_WORKSPACE_JOIN_REQUEST_BYTES)
            .await
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        let request = postcard::from_bytes(&bytes)
            .map_err(|error| WorkspaceJoinError::Decode(error.to_string()))?;
        let response = self.service.handle(remote_endpoint, request).await;
        let bytes = postcard::to_stdvec(&response)
            .map_err(|error| WorkspaceJoinError::Encode(error.to_string()))?;
        if bytes.len() > MAX_WORKSPACE_JOIN_RESPONSE_BYTES {
            return Err(WorkspaceJoinError::Protocol(
                "workspace join response exceeds the wire limit".into(),
            ));
        }
        crate::framing::write_frame(&mut send, &bytes)
            .await
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        send.finish()
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        connection.closed().await;
        Ok(())
    }
}

/// Typed client for the shared invitation and membership exchange.
#[derive(Clone, Debug)]
pub struct WorkspaceJoinClient {
    endpoint: iroh::Endpoint,
}

impl WorkspaceJoinClient {
    /// Use an existing identity-bound Iroh endpoint.
    #[must_use]
    pub fn new(endpoint: iroh::Endpoint) -> Self {
        Self { endpoint }
    }

    /// Send one bounded typed request over a fresh bidirectional stream.
    pub async fn request(
        &self,
        address: iroh::EndpointAddr,
        request: JoinTransportRequest,
    ) -> Result<JoinTransportResponse, WorkspaceJoinError> {
        n0_future::time::timeout(EXCHANGE_TIMEOUT, self.request_inner(address, request))
            .await
            .map_err(|_| WorkspaceJoinError::Timeout)?
    }

    async fn request_inner(
        &self,
        address: iroh::EndpointAddr,
        request: JoinTransportRequest,
    ) -> Result<JoinTransportResponse, WorkspaceJoinError> {
        let bytes = postcard::to_stdvec(&request)
            .map_err(|error| WorkspaceJoinError::Encode(error.to_string()))?;
        if bytes.len() > MAX_WORKSPACE_JOIN_REQUEST_BYTES {
            return Err(WorkspaceJoinError::Protocol(
                "workspace join request exceeds the wire limit".into(),
            ));
        }
        let connection = self
            .endpoint
            .connect(address, WORKSPACE_JOIN_ALPN)
            .await
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        crate::framing::write_frame(&mut send, &bytes)
            .await
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        send.finish()
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        let response = crate::framing::read_frame(&mut recv, MAX_WORKSPACE_JOIN_RESPONSE_BYTES)
            .await
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        postcard::from_bytes(&response)
            .map_err(|error| WorkspaceJoinError::Decode(error.to_string()))
    }

    /// Verify and inspect an invitation through its issuing host without consuming it.
    pub async fn inspect(
        &self,
        address: iroh::EndpointAddr,
        signed_invitation: Vec<u8>,
    ) -> Result<qos_peer_state::join::JoinInspection, WorkspaceJoinError> {
        match self
            .request(address, JoinTransportRequest::Inspect { signed_invitation })
            .await?
        {
            JoinTransportResponse::Inspection(inspection) => Ok(inspection),
            response => Err(response_error(response)),
        }
    }

    /// Run challenge, device proof of possession, and invitation redemption.
    pub async fn redeem(
        &self,
        address: iroh::EndpointAddr,
        signed_invitation: Vec<u8>,
        device_cert: DeviceCert,
        device_key: &ed25519_dalek::SigningKey,
        request_id: [u8; 32],
    ) -> Result<JoinBootstrap, WorkspaceJoinError> {
        if *self.endpoint.id().as_bytes() != device_cert.device_public_key
            || device_key.verifying_key().to_bytes() != device_cert.device_public_key
        {
            return Err(WorkspaceJoinError::Protocol(
                "join client endpoint, certificate, and proof key disagree".into(),
            ));
        }
        let challenge = match self
            .request(
                address.clone(),
                JoinTransportRequest::Challenge(JoinChallengeRequest {
                    signed_invitation,
                    device_cert: device_cert.clone(),
                    request_id,
                }),
            )
            .await?
        {
            JoinTransportResponse::Challenge(response) => response.challenge,
            response => return Err(response_error(response)),
        };
        let proof = sign_join_proof(challenge, device_key).map_err(WorkspaceJoinError::Protocol)?;
        match self
            .request(
                address,
                JoinTransportRequest::Complete(qos_peer_state::join::JoinCompleteRequest { proof }),
            )
            .await?
        {
            JoinTransportResponse::Complete(bootstrap) => Ok(bootstrap),
            response => Err(response_error(response)),
        }
    }

    /// Verify the signed host/service binding and redeem through its exact pinned address.
    pub async fn redeem_hint(
        &self,
        hint: &TransportHint,
        signed_invitation: Vec<u8>,
        device_cert: DeviceCert,
        device_key: &ed25519_dalek::SigningKey,
        request_id: [u8; 32],
    ) -> Result<WorkspaceJoinAdmission, WorkspaceJoinError> {
        let host = IrohWorkspaceHostHintV1::from_transport_hint(hint)?;
        let inspection = self
            .inspect(host.endpoint_addr.clone(), signed_invitation.clone())
            .await?;
        let invitation_trust = host.validate_invitation(&inspection.grant)?;
        let bootstrap = self
            .redeem(
                host.endpoint_addr.clone(),
                signed_invitation,
                device_cert,
                device_key,
                request_id,
            )
            .await?;
        let trust = host.validate_bootstrap(&bootstrap)?;
        if trust != invitation_trust {
            return Err(WorkspaceJoinError::Protocol(
                "invitation and bootstrap host trust disagree".into(),
            ));
        }
        Ok(WorkspaceJoinAdmission { bootstrap, trust })
    }

    /// Refresh bootstrap state for an already admitted member without reusing an invitation.
    #[allow(
        clippy::too_many_arguments,
        reason = "membership proof inputs remain explicit so no caller-supplied identity can hide in an options bag"
    )]
    pub async fn refresh_membership(
        &self,
        address: iroh::EndpointAddr,
        room: String,
        workspace_id: u64,
        device_cert: DeviceCert,
        device_key: &ed25519_dalek::SigningKey,
        request_id: [u8; 32],
    ) -> Result<JoinBootstrap, WorkspaceJoinError> {
        if *self.endpoint.id().as_bytes() != device_cert.device_public_key
            || device_key.verifying_key().to_bytes() != device_cert.device_public_key
        {
            return Err(WorkspaceJoinError::Protocol(
                "membership client endpoint, certificate, and proof key disagree".into(),
            ));
        }
        let challenge = match self
            .request(
                address.clone(),
                JoinTransportRequest::MembershipChallenge(MembershipChallengeRequest {
                    room,
                    workspace_id,
                    device_cert,
                    request_id,
                }),
            )
            .await?
        {
            JoinTransportResponse::MembershipChallenge(response) => response.challenge,
            response => return Err(response_error(response)),
        };
        let proof =
            sign_membership_proof(challenge, device_key).map_err(WorkspaceJoinError::Protocol)?;
        match self
            .request(
                address,
                JoinTransportRequest::MembershipComplete(MembershipCompleteRequest { proof }),
            )
            .await?
        {
            JoinTransportResponse::MembershipComplete(bootstrap) => Ok(bootstrap),
            response => Err(response_error(response)),
        }
    }

    /// Verify the signed host/service binding and refresh through its exact pinned address.
    #[allow(
        clippy::too_many_arguments,
        reason = "verified hint plus explicit membership proof inputs preserve the transport trust boundary"
    )]
    pub async fn refresh_membership_hint(
        &self,
        hint: &TransportHint,
        room: String,
        workspace_id: u64,
        device_cert: DeviceCert,
        device_key: &ed25519_dalek::SigningKey,
        request_id: [u8; 32],
    ) -> Result<WorkspaceJoinAdmission, WorkspaceJoinError> {
        let host = IrohWorkspaceHostHintV1::from_transport_hint(hint)?;
        if host.room != room || host.workspace_id != workspace_id {
            return Err(WorkspaceJoinError::Protocol(
                "membership scope does not match its signed workspace host hint".into(),
            ));
        }
        let bootstrap = self
            .refresh_membership(
                host.endpoint_addr.clone(),
                room,
                workspace_id,
                device_cert,
                device_key,
                request_id,
            )
            .await?;
        let trust = host.validate_bootstrap(&bootstrap)?;
        Ok(WorkspaceJoinAdmission { bootstrap, trust })
    }
}

fn response_error(response: JoinTransportResponse) -> WorkspaceJoinError {
    match response {
        JoinTransportResponse::Error { code, message } => {
            WorkspaceJoinError::Remote { code, message }
        }
        _ => WorkspaceJoinError::Protocol("workspace join response variant mismatch".into()),
    }
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use qos_peer_state::join::{JoinTransportRequest, JoinTransportResponse};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, Default)]
    struct Capture {
        requests: Arc<Mutex<Vec<([u8; 32], JoinTransportRequest)>>>,
    }

    impl WorkspaceJoinService for Capture {
        async fn handle(
            &self,
            remote_endpoint: [u8; 32],
            request: JoinTransportRequest,
        ) -> JoinTransportResponse {
            self.requests
                .lock()
                .unwrap()
                .push((remote_endpoint, request));
            JoinTransportResponse::Error {
                code: 409,
                message: "captured".into(),
            }
        }
    }

    #[tokio::test]
    async fn typed_exchange_binds_request_to_authenticated_remote_endpoint() {
        let service = Capture::default();
        let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let router = iroh::protocol::Router::builder(server.clone())
            .accept(
                WORKSPACE_JOIN_ALPN,
                WorkspaceJoinProtocol::new(service.clone()),
            )
            .spawn();
        let client_endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let client = WorkspaceJoinClient::new(client_endpoint.clone());
        let request = JoinTransportRequest::Inspect {
            signed_invitation: vec![3, 1, 4, 1, 5],
        };

        let response = client
            .request(
                iroh::EndpointAddr::new(server.id()).with_ip_addr(std::net::SocketAddr::from((
                    std::net::Ipv4Addr::LOCALHOST,
                    server.bound_sockets().first().unwrap().port(),
                ))),
                request.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            response,
            JoinTransportResponse::Error {
                code: 409,
                message: "captured".into(),
            }
        );
        assert_eq!(
            service.requests.lock().unwrap().as_slice(),
            &[(*client_endpoint.id().as_bytes(), request)]
        );

        router.shutdown().await.unwrap();
        client_endpoint.close().await;
    }
}

mod delivery_origin;
pub use delivery_origin::{DeliveryOriginLocatorV1, TRANSPORT_DELIVERY_ORIGIN_V1};
