//! The client half of the owner-neutral Iroh transport for the shared
//! workspace join contract. The relay's side, `WorkspaceJoinProtocol`, stays
//! in `tilo-relay`; the messages are `tilo_peer_state::join`'s.

use std::time::Duration;
use tilo_delivery_protocol::workspace_join::{
    IrohWorkspaceHostHintV1, MAX_WORKSPACE_JOIN_REQUEST_BYTES, MAX_WORKSPACE_JOIN_RESPONSE_BYTES,
    WORKSPACE_JOIN_ALPN, WorkspaceJoinAdmission, WorkspaceJoinError,
};
use tilo_peer_state::join::{
    JoinBootstrap, JoinChallengeRequest, JoinTransportRequest, JoinTransportResponse,
    MembershipChallengeRequest, MembershipCompleteRequest, TransportHint, sign_join_proof,
    sign_membership_proof,
};
use tilo_user_identity::DeviceCert;

/// How long one join exchange may take, on either side.
pub const WORKSPACE_JOIN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

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
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the request cannot be sent or its reply is rejected.
    pub async fn request(
        &self,
        address: iroh::EndpointAddr,
        request: JoinTransportRequest,
    ) -> Result<JoinTransportResponse, WorkspaceJoinError> {
        n0_future::time::timeout(
            WORKSPACE_JOIN_EXCHANGE_TIMEOUT,
            self.request_inner(address, request),
        )
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
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the request cannot be sent or its reply is rejected.
    pub async fn inspect(
        &self,
        address: iroh::EndpointAddr,
        signed_invitation: Vec<u8>,
    ) -> Result<tilo_peer_state::join::JoinInspection, WorkspaceJoinError> {
        match self
            .request(address, JoinTransportRequest::Inspect { signed_invitation })
            .await?
        {
            JoinTransportResponse::Inspection(inspection) => Ok(inspection),
            response => Err(response_error(response)),
        }
    }

    /// Run challenge, device proof of possession, and invitation redemption.
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the request cannot be sent or its reply is rejected.
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
                JoinTransportRequest::Complete(tilo_peer_state::join::JoinCompleteRequest {
                    proof,
                }),
            )
            .await?
        {
            JoinTransportResponse::Complete(bootstrap) => Ok(bootstrap),
            response => Err(response_error(response)),
        }
    }

    /// Verify the signed host/service binding and redeem through its exact pinned address.
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the request cannot be sent or its reply is rejected.
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
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the request cannot be sent or its reply is rejected.
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
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the request cannot be sent or its reply is rejected.
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
