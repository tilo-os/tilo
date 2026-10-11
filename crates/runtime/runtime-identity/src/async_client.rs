//! Async identity-broker client for host-owned app transports.

#![allow(
    clippy::future_not_send,
    reason = "host transports and their identity client are confined to the SDK worker LocalSet"
)]

use std::io;
use std::pin::Pin;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use wire_frame::FrameError;

use crate::client::{BrokerMetadata, channel_keys_reply, connection_failed, signature_reply};
use crate::{NotAvailable, wire};

trait BrokerAsyncIo: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite> BrokerAsyncIo for T {}

type BrokerStream = Pin<Box<dyn BrokerAsyncIo>>;

/// Public identity metadata plus a serialized async signing connection.
///
/// The host supplies an already authenticated and route-bound transport. This
/// client owns no private key and never falls back to local identity state.
/// Every request/response exchange has one absolute deadline covering the
/// complete write, flush, frame read, and decode. A transport or protocol
/// failure drops the stream so a late reply can never satisfy another request.
pub struct AsyncBrokerSigningIdentity {
    metadata: BrokerMetadata,
    stream: Option<BrokerStream>,
    exchange_timeout: Duration,
}

impl std::fmt::Debug for AsyncBrokerSigningIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncBrokerSigningIdentity")
            .finish_non_exhaustive()
    }
}

impl AsyncBrokerSigningIdentity {
    /// Request metadata over a caller-owned connected async transport.
    ///
    /// `exchange_timeout` applies independently to metadata and each later
    /// signing request. The caller remains responsible for authenticating the
    /// endpoint and binding it to the app and workspace before construction.
    ///
    /// # Errors
    /// Returns a bounded transport, framing, protocol, or broker refusal error.
    pub async fn from_connected_io<S>(
        stream: S,
        exchange_timeout: Duration,
    ) -> Result<Self, NotAvailable>
    where
        S: AsyncRead + AsyncWrite + 'static,
    {
        let mut stream: BrokerStream = Box::pin(stream);
        let reply = exchange(&mut stream, &wire::Request::Metadata, exchange_timeout).await?;
        Ok(Self {
            metadata: BrokerMetadata::from_reply(reply)?,
            stream: Some(stream),
            exchange_timeout,
        })
    }

    /// The public metadata the host reported when this connection opened.
    pub fn metadata(&self) -> &BrokerMetadata {
        &self.metadata
    }

    /// Whether another request can safely use the framed stream.
    ///
    /// Complete broker refusals retain alignment and return `true`. Transport,
    /// framing, timeout, and protocol failures drop the stream and return
    /// `false`, allowing the owning session to fail closed immediately.
    pub fn is_usable(&self) -> bool {
        self.stream.is_some()
    }

    /// Ask the host for a certified collaboration-join proof.
    ///
    /// # Errors
    /// Returns a broker refusal or a bounded fail-closed transport error.
    pub async fn sign_collaboration_join(
        &mut self,
        workspace_id: &str,
    ) -> Result<[u8; 64], NotAvailable> {
        self.signature(&wire::Request::CollaborationJoin {
            workspace_id: workspace_id.to_owned(),
        })
        .await
    }

    /// Ask the host to sign one typed collaboration operation.
    ///
    /// # Errors
    /// Returns a broker refusal or a bounded fail-closed transport error.
    pub async fn sign_collaboration_op(
        &mut self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        unsigned_op: &[u8],
    ) -> Result<[u8; 64], NotAvailable> {
        self.signature(&wire::Request::CollaborationOp {
            workspace_id: workspace_id.to_owned(),
            app_id: app_id.to_owned(),
            record_id: record_id.to_owned(),
            unsigned_op: unsigned_op.to_vec(),
        })
        .await
    }

    /// Ask the host for the content keys of one document route.
    ///
    /// # Errors
    /// Returns a broker refusal or a bounded fail-closed transport error.
    pub async fn channel_keys(
        &mut self,
        workspace_id: &str,
        app_id: &str,
        record_id: &str,
        known_generation: u64,
    ) -> Result<wire::ChannelKeysReply, NotAvailable> {
        let response = self
            .exchange(&wire::Request::ChannelKeys {
                workspace_id: workspace_id.to_owned(),
                app_id: app_id.to_owned(),
                record_id: record_id.to_owned(),
                known_generation,
            })
            .await?;
        self.unless_invalid(channel_keys_reply(response))
    }

    async fn signature(&mut self, request: &wire::Request) -> Result<[u8; 64], NotAvailable> {
        let response = self.exchange(request).await?;
        self.unless_invalid(signature_reply(response))
    }

    /// Exchange one request; a transport, framing or timeout failure drops
    /// the stream so a late reply can never satisfy another request.
    async fn exchange(&mut self, request: &wire::Request) -> Result<wire::Response, NotAvailable> {
        let mut stream = self.stream.take().ok_or_else(poisoned)?;
        let response = exchange(&mut stream, request, self.exchange_timeout).await?;
        self.stream = Some(stream);
        Ok(response)
    }

    /// A reply the protocol does not allow leaves the stream unaligned.
    fn unless_invalid<T>(&mut self, reply: Result<T, NotAvailable>) -> Result<T, NotAvailable> {
        if matches!(&reply, Err(NotAvailable::ConnectionFailed(error)) if error.kind() == io::ErrorKind::InvalidData)
        {
            self.poison();
        }
        reply
    }

    fn poison(&mut self) {
        self.stream = None;
    }
}

/// Write `request` and read its response within one absolute deadline
/// covering the write, flush, frame read and decode.
async fn exchange(
    stream: &mut BrokerStream,
    request: &wire::Request,
    timeout: Duration,
) -> Result<wire::Response, NotAvailable> {
    // A timeout too large to represent as an instant is a deadline that
    // never elapses, not a reason to panic.
    let deadline = tokio::time::Instant::now().checked_add(timeout);
    let exchange = async {
        write_frame(stream, request).await?;
        read_frame(stream).await
    };
    let result = match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, exchange).await,
        None => Ok(exchange.await),
    };
    match result {
        Ok(Ok(response))
            if deadline.is_none_or(|deadline| tokio::time::Instant::now() < deadline) =>
        {
            Ok(response)
        }
        Ok(Ok(_)) | Err(_) => Err(NotAvailable::ConnectionFailed(io::Error::new(
            io::ErrorKind::TimedOut,
            "identity broker exchange timed out",
        ))),
        Ok(Err(error)) => Err(error),
    }
}

async fn write_frame<T: serde::Serialize + Sync>(
    stream: &mut BrokerStream,
    value: &T,
) -> Result<(), NotAvailable> {
    wire::FRAMING
        .write_value_async(stream, value)
        .await
        .map_err(connection_failed)?;
    stream.flush().await?;
    Ok(())
}

async fn read_frame(stream: &mut BrokerStream) -> Result<wire::Response, NotAvailable> {
    wire::FRAMING
        .read_value_async(stream)
        .await
        .map_err(connection_failed)?
        .ok_or_else(|| connection_failed(FrameError::Closed))
}

fn poisoned() -> NotAvailable {
    NotAvailable::ConnectionFailed(io::Error::new(
        io::ErrorKind::BrokenPipe,
        "identity broker connection is no longer usable",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt as _;

    fn metadata() -> wire::Response {
        wire::Response::Metadata {
            display_name: "Async fixture".into(),
            avatar_color: [1, 2, 3],
            group_public_key: [4; 32],
            signing_public_key: [5; 32],
            device_certificate: Some(b"fixture certificate".to_vec()),
            threshold: 1,
            total_shares: 1,
            share_index: 1,
        }
    }

    async fn read_request(stream: &mut tokio::io::DuplexStream) -> wire::Request {
        wire::FRAMING
            .read_value_async(stream)
            .await
            .unwrap()
            .unwrap()
    }

    async fn write_response(stream: &mut tokio::io::DuplexStream, response: wire::Response) {
        wire::FRAMING
            .write_value_async(stream, &response)
            .await
            .unwrap();
    }

    fn error_kind<T>(result: Result<T, NotAvailable>) -> io::ErrorKind {
        match result {
            Err(NotAvailable::ConnectionFailed(error)) => error.kind(),
            _ => panic!("expected connection failure"),
        }
    }

    async fn connected(timeout: Duration) -> (AsyncBrokerSigningIdentity, tokio::io::DuplexStream) {
        let (client, mut server) = tokio::io::duplex(4096);
        let server_task = tokio::spawn(async move {
            assert!(matches!(
                read_request(&mut server).await,
                wire::Request::Metadata
            ));
            write_response(&mut server, metadata()).await;
            server
        });
        let identity = AsyncBrokerSigningIdentity::from_connected_io(client, timeout)
            .await
            .unwrap();
        (identity, server_task.await.unwrap())
    }

    async fn assert_raw_response_poisoned(bytes: Vec<u8>, expected: io::ErrorKind) {
        let (mut identity, mut server) = connected(Duration::from_secs(1)).await;
        let broker = tokio::spawn(async move {
            assert!(matches!(
                read_request(&mut server).await,
                wire::Request::CollaborationJoin { .. }
            ));
            server.write_all(&bytes).await.unwrap();
            server.shutdown().await.unwrap();
        });
        assert_eq!(
            error_kind(identity.sign_collaboration_join("workspace").await),
            expected
        );
        assert!(!identity.is_usable());
        assert_eq!(
            error_kind(
                identity
                    .sign_collaboration_op("workspace", "app", "record", b"op")
                    .await
            ),
            io::ErrorKind::BrokenPipe
        );
        broker.await.unwrap();
    }

    #[tokio::test]
    async fn the_async_client_speaks_the_blocking_wire_and_keeps_alignment() {
        // phase: async_wire_bytes_are_pinned
        // The async client speaks the same bytes as the blocking one: a
        // little-endian `u32` length, then postcard.
        {
            const REQUEST: [u8; 7] = [3, 0, 0, 0, 2, 1, b'w'];
            let (mut identity, mut server) = connected(Duration::from_secs(5)).await;
            let broker = tokio::spawn(async move {
                let mut request = [0_u8; REQUEST.len()];
                server.read_exact(&mut request).await.unwrap();
                let mut response = vec![66, 0, 0, 0, 1, 64];
                response.extend_from_slice(&[7; 64]);
                server.write_all(&response).await.unwrap();
                request
            });
            assert_eq!(
                identity.sign_collaboration_join("w").await.unwrap(),
                [7; 64]
            );
            assert_eq!(broker.await.unwrap(), REQUEST);
        }
        // phase: unrepresentable_timeout_waits_without_deadline
        {
            let (mut identity, mut server) = connected(Duration::MAX).await;
            let broker = tokio::spawn(async move {
                assert!(matches!(
                    read_request(&mut server).await,
                    wire::Request::CollaborationJoin { .. }
                ));
                write_response(&mut server, wire::Response::Signature(vec![9; 64])).await;
            });
            assert_eq!(
                identity.sign_collaboration_join("workspace").await.unwrap(),
                [9; 64]
            );
            assert!(identity.is_usable());
            broker.await.unwrap();
        }
        // phase: complete_denial_preserves_alignment_for_next_request
        {
            let (mut identity, mut server) = connected(Duration::from_secs(1)).await;
            let broker = tokio::spawn(async move {
                assert!(matches!(
                    read_request(&mut server).await,
                    wire::Request::CollaborationJoin { .. }
                ));
                write_response(&mut server, wire::Response::Denied("revoked".into())).await;
                assert!(matches!(
                    read_request(&mut server).await,
                    wire::Request::CollaborationOp { .. }
                ));
                write_response(&mut server, wire::Response::Signature(vec![9; 64])).await;
            });
            assert_eq!(
                error_kind(identity.sign_collaboration_join("workspace").await),
                io::ErrorKind::PermissionDenied
            );
            assert!(identity.is_usable());
            assert_eq!(
                identity
                    .sign_collaboration_op("workspace", "app", "record", b"op")
                    .await
                    .unwrap(),
                [9; 64]
            );
            broker.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_exchanges_time_out_poison_and_never_block_other_work() {
        // phase: stalled_metadata_obeys_one_absolute_deadline
        {
            let (client, mut server) = tokio::io::duplex(64);
            tokio::spawn(async move {
                assert!(matches!(
                    read_request(&mut server).await,
                    wire::Request::Metadata
                ));
                std::future::pending::<()>().await;
            });
            assert_eq!(
                error_kind(
                    AsyncBrokerSigningIdentity::from_connected_io(
                        client,
                        Duration::from_millis(20)
                    )
                    .await
                ),
                io::ErrorKind::TimedOut
            );
        }
        // phase: stalled_join_poison_drops_late_reply_before_next_exchange
        {
            let (mut identity, mut server) = connected(Duration::from_millis(20)).await;
            let late = tokio::spawn(async move {
                assert!(matches!(
                    read_request(&mut server).await,
                    wire::Request::CollaborationJoin { .. }
                ));
                tokio::time::sleep(Duration::from_millis(40)).await;
                wire::FRAMING
                    .write_value_async(&mut server, &wire::Response::Signature(vec![7; 64]))
                    .await
                    .map_err(std::io::Error::from)
            });
            assert_eq!(
                error_kind(identity.sign_collaboration_join("workspace").await),
                io::ErrorKind::TimedOut
            );
            assert!(!identity.is_usable());
            assert_eq!(
                error_kind(
                    identity
                        .sign_collaboration_op("workspace", "app", "record", b"op")
                        .await
                ),
                io::ErrorKind::BrokenPipe
            );
            assert!(
                late.await.unwrap().is_err(),
                "timed-out stream must be closed"
            );
        }
        // phase: stalled_operation_keeps_localset_work_responsive
        {
            let (mut identity, mut server) = connected(Duration::from_millis(20)).await;
            let (request_seen, seen) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                assert!(matches!(
                    read_request(&mut server).await,
                    wire::Request::CollaborationOp { .. }
                ));
                let _ = request_seen.send(());
                std::future::pending::<()>().await;
            });
            let (responsive, response) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                seen.await.unwrap();
                tokio::task::yield_now().await;
                let _ = responsive.send(());
            });
            let operation = identity.sign_collaboration_op("workspace", "app", "record", b"op");
            tokio::pin!(operation);
            tokio::select! {
                result = &mut operation => {
                    assert_eq!(error_kind(result), io::ErrorKind::TimedOut);
                }
                result = response => {
                    result.unwrap();
                    assert_eq!(
                        error_kind(operation.await),
                        io::ErrorKind::TimedOut
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn a_bad_raw_response_poisons_the_client_against_reuse() {
        let oversized = u32::try_from(wire::FRAMING.max_len() + 1)
            .unwrap()
            .to_le_bytes();
        // (case, raw response bytes, error kind)
        let cases = [
            ("oversized", oversized.to_vec(), io::ErrorKind::InvalidData),
            ("partial header", vec![1, 0], io::ErrorKind::UnexpectedEof),
            (
                "partial body",
                vec![2, 0, 0, 0, 1],
                io::ErrorKind::UnexpectedEof,
            ),
            (
                "malformed postcard",
                vec![1, 0, 0, 0, 255],
                io::ErrorKind::InvalidData,
            ),
        ];
        for (case, bytes, kind) in cases {
            eprintln!("case: {case}");
            assert_raw_response_poisoned(bytes, kind).await;
        }
    }
}
