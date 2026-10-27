//! The owner-neutral iroh transport for the shared workspace join contract.
//! The client half reaches a governor over its iroh endpoint, or over its
//! HTTPS route for a host that publishes no iroh address. The server half,
//! [`WorkspaceJoinProtocol`], is how every governor answers on its iroh
//! endpoint: a relay for the rooms it governs and a device for its own. The
//! HTTPS route stays in `tilo-relay`; the messages are
//! `tilo_workspace_join`'s.
//!
//! An invitation a host's admission door issued comes with the door's
//! one-time token. Over HTTPS the client spends it as a page does
//! (`JoinStep::ProvePreAdmitted`, under the door's limits and with the
//! notice the room keeps) before it completes; the completion then answers
//! that admission's bootstrap.

use std::sync::Arc;
use std::time::Duration;
use tilo_delivery_protocol::workspace_join::{
    IrohWorkspaceHostHintV1, MAX_WORKSPACE_JOIN_REQUEST_BYTES, MAX_WORKSPACE_JOIN_RESPONSE_BYTES,
    WORKSPACE_JOIN_ALPN, WorkspaceJoinAdmission, WorkspaceJoinError,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_join::{
    JoinBootstrap, JoinChallengeRequest, JoinTransportRequest, JoinTransportResponse,
    TransportHint, sign_join_proof,
};

/// How long one join exchange may take, on either side.
pub const WORKSPACE_JOIN_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// Typed client for the shared invitation and membership exchange.
#[derive(Clone, Debug)]
pub struct WorkspaceJoinClient {
    carrier: Carrier,
}

#[derive(Clone)]
enum Carrier {
    /// The host's iroh endpoint, dialled from an identity-bound endpoint.
    Iroh(iroh::Endpoint),
    /// The host's HTTPS route, with the door's admission when it gave one.
    Https {
        http: Arc<dyn tilo_relay_client::HttpClient>,
        door: Option<DoorAdmission>,
    },
}

impl std::fmt::Debug for Carrier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Iroh(endpoint) => f.debug_tuple("Iroh").field(&endpoint.id()).finish(),
            Self::Https { door, .. } => f
                .debug_struct("Https")
                .field("door", &door.is_some())
                .finish(),
        }
    }
}

/// What a host's admission door gave this device's person beside its
/// invitation: the one-time token that admits them at once, and the name
/// they give the room.
#[derive(Clone)]
pub struct DoorAdmission {
    pub token: Vec<u8>,
    /// How the person names themself: a display name.
    pub name: String,
}

impl WorkspaceJoinClient {
    /// Use an existing identity-bound Iroh endpoint.
    #[must_use]
    pub fn new(endpoint: iroh::Endpoint) -> Self {
        Self {
            carrier: Carrier::Iroh(endpoint),
        }
    }

    /// Use the HTTPS route of the host behind `http`, spending `door`'s
    /// admission before completing, when the invitation came from a door.
    /// The address a request names is then the host's identity only: the
    /// route reaches the same host.
    #[must_use]
    pub fn https(
        http: Arc<dyn tilo_relay_client::HttpClient>,
        door: Option<DoorAdmission>,
    ) -> Self {
        Self {
            carrier: Carrier::Https { http, door },
        }
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
        let endpoint = match &self.carrier {
            Carrier::Iroh(endpoint) => endpoint,
            Carrier::Https { http, door } => {
                if let (JoinTransportRequest::Complete(complete), Some(door)) = (&request, door) {
                    spend_door_admission(http.as_ref(), complete, door).await?;
                }
                let response = tilo_relay_client::https::workspace_join(http.as_ref(), bytes)
                    .await
                    .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
                if response.len() > MAX_WORKSPACE_JOIN_RESPONSE_BYTES {
                    return Err(WorkspaceJoinError::Protocol(
                        "workspace join response exceeds the wire limit".into(),
                    ));
                }
                return postcard::from_bytes(&response)
                    .map_err(|error| WorkspaceJoinError::Decode(error.to_string()));
            }
        };
        let connection = endpoint
            .connect(address, WORKSPACE_JOIN_ALPN)
            .await
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        tilo_relay_client::frame::write_frame(&mut send, &bytes)
            .await
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        send.finish()
            .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
        let response =
            tilo_relay_client::frame::read_frame(&mut recv, MAX_WORKSPACE_JOIN_RESPONSE_BYTES)
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
    ) -> Result<tilo_workspace_join::JoinInspection, WorkspaceJoinError> {
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
        // Over iroh the host binds the request to the dialling endpoint; over
        // HTTPS to the device the request names.
        let endpoint_disagrees = match &self.carrier {
            Carrier::Iroh(endpoint) => *endpoint.id().as_bytes() != device_cert.device_public_key,
            Carrier::Https { .. } => false,
        };
        if endpoint_disagrees
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
                JoinTransportRequest::Complete(tilo_workspace_join::JoinCompleteRequest { proof }),
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
}

/// Spend `door`'s admission with the proof `complete` carries, as a page
/// does, so the completion that follows answers that admission. A proof
/// spent before (an exact retry) is admitted already, which the host's
/// status of the request says.
async fn spend_door_admission(
    http: &dyn tilo_relay_client::HttpClient,
    complete: &tilo_workspace_join::JoinCompleteRequest,
    door: &DoorAdmission,
) -> Result<(), WorkspaceJoinError> {
    use tilo_relay_protocol::rooms::{JoinReply, JoinStep};
    let transport =
        |error: tilo_relay_client::LinkError| WorkspaceJoinError::Transport(error.to_string());
    let challenge = postcard::to_stdvec(&complete.proof.challenge)
        .map_err(|error| WorkspaceJoinError::Encode(error.to_string()))?;
    let reply = tilo_relay_client::https::join(
        http,
        &JoinStep::ProvePreAdmitted {
            challenge,
            signature: complete.proof.signature,
            name: door.name.clone(),
            token: door.token.clone(),
        },
    )
    .await
    .map_err(transport)?;
    let refusal = match reply {
        JoinReply::Admitted { .. } => return Ok(()),
        JoinReply::Refused(reason) => reason,
        other => format!("the door's admission was answered {other:?}"),
    };
    let status = tilo_relay_client::https::join(
        http,
        &JoinStep::Status {
            request_id: complete.proof.challenge.request_id,
            device_public_key: complete.proof.challenge.device_public_key,
        },
    )
    .await
    .map_err(transport)?;
    if matches!(status, JoinReply::Admitted { .. }) {
        return Ok(());
    }
    Err(WorkspaceJoinError::Remote {
        code: 403,
        message: refusal,
    })
}

fn response_error(response: JoinTransportResponse) -> WorkspaceJoinError {
    match response {
        JoinTransportResponse::Error { code, message } => {
            WorkspaceJoinError::Remote { code, message }
        }
        _ => WorkspaceJoinError::Protocol("workspace join response variant mismatch".into()),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use server::{WorkspaceJoinProtocol, bounded_accept};

/// The server half: one bounded postcard request and reply per
/// bidirectional stream, bound to the authenticated remote endpoint.
#[cfg(not(target_arch = "wasm32"))]
mod server {
    use iroh::protocol::{AcceptError, ProtocolHandler};
    use std::sync::Arc;
    use tilo_delivery_protocol::workspace_join::{
        MAX_WORKSPACE_JOIN_REQUEST_BYTES, MAX_WORKSPACE_JOIN_RESPONSE_BYTES, WorkspaceJoinError,
        WorkspaceJoinService,
    };

    use super::WORKSPACE_JOIN_EXCHANGE_TIMEOUT;

    const MAX_CONCURRENT_EXCHANGES: usize = 64;

    /// Iroh protocol handler that provides bounded postcard request/response framing.
    #[derive(Clone, Debug)]
    pub struct WorkspaceJoinProtocol<S> {
        service: S,
        exchanges: Arc<tokio::sync::Semaphore>,
    }

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

    impl<S: WorkspaceJoinService> ProtocolHandler for WorkspaceJoinProtocol<S> {
        async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), AcceptError> {
            bounded_accept(
                &self.exchanges,
                WORKSPACE_JOIN_EXCHANGE_TIMEOUT,
                "workspace join exchange",
                self.exchange(&connection),
            )
            .await
        }
    }

    /// Run one `what` exchange under a slot of `exchanges` and `timeout`,
    /// refusing it outright when every slot is taken.
    pub async fn bounded_accept<E: std::fmt::Display>(
        exchanges: &Arc<tokio::sync::Semaphore>,
        timeout: std::time::Duration,
        what: &str,
        exchange: impl Future<Output = Result<(), E>>,
    ) -> Result<(), AcceptError> {
        let _permit = exchanges.clone().try_acquire_owned().map_err(|_| {
            AcceptError::from_err(std::io::Error::other(format!(
                "{what} concurrency limit reached"
            )))
        })?;
        n0_future::time::timeout(timeout, exchange)
            .await
            .map_err(|_| {
                AcceptError::from_err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{what} timed out"),
                ))
            })?
            .map_err(|error| AcceptError::from_err(std::io::Error::other(error.to_string())))
    }

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
            let bytes =
                tilo_relay_client::frame::read_frame(&mut recv, MAX_WORKSPACE_JOIN_REQUEST_BYTES)
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
            tilo_relay_client::frame::write_frame(&mut send, &bytes)
                .await
                .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
            send.finish()
                .map_err(|error| WorkspaceJoinError::Transport(error.to_string()))?;
            connection.closed().await;
            Ok(())
        }
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN;
    use tilo_delivery_protocol::workspace_join::WorkspaceJoinService;
    use tilo_workspace_join::{JoinTransportRequest, JoinTransportResponse};

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
