//! Owner-neutral Iroh transport for the shared workspace join contract: the
//! host's [`WorkspaceJoinProtocol`]. The client half,
//! [`WorkspaceJoinClient`], lives in `tilo-relay-session` and is re-exported
//! here under its historic path.

#[cfg(not(target_arch = "wasm32"))]
use iroh::protocol::{AcceptError, ProtocolHandler};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use tilo_delivery_protocol::workspace_join::{
    MAX_WORKSPACE_JOIN_REQUEST_BYTES, MAX_WORKSPACE_JOIN_RESPONSE_BYTES, WorkspaceJoinError,
    WorkspaceJoinService,
};
#[cfg(not(target_arch = "wasm32"))]
use tilo_relay_session::workspace_join::WORKSPACE_JOIN_EXCHANGE_TIMEOUT as EXCHANGE_TIMEOUT;

pub use tilo_relay_session::workspace_join::WorkspaceJoinClient;

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
        bounded_accept(
            &self.exchanges,
            EXCHANGE_TIMEOUT,
            "workspace join exchange",
            self.exchange(&connection),
        )
        .await
    }
}

/// Run one `what` exchange under a slot of `exchanges` and `timeout`,
/// refusing it outright when every slot is taken.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn bounded_accept<E: std::fmt::Display>(
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

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tilo_delivery_protocol::workspace_join::WORKSPACE_JOIN_ALPN;
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
