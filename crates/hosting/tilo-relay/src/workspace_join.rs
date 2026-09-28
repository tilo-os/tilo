//! Owner-neutral Iroh transport for the shared workspace join contract.

#[cfg(not(target_arch = "wasm32"))]
use iroh::protocol::{AcceptError, ProtocolHandler};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use tilo_delivery_protocol::workspace_join::WorkspaceJoinService;
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

const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(target_arch = "wasm32"))]
const MAX_CONCURRENT_EXCHANGES: usize = 64;

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
    ///
    /// # Errors
    ///
    /// Returns `WorkspaceJoinError` if the request cannot be sent or its reply is rejected.
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tilo_peer_state::join::{JoinTransportRequest, JoinTransportResponse};

    type CapturedRequest = ([u8; 32], JoinTransportRequest);

    #[derive(Clone, Debug, Default)]
    struct Capture {
        requests: Arc<Mutex<Vec<CapturedRequest>>>,
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
