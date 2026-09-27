//! Bounded local share-management protocol for Files.
//!
//! The listener is supplied by the daemon so socket placement and `0600`
//! permissions remain deployment policy. This module owns only strict framing,
//! bounded concurrency, exact record admission and coarse responses. Secret
//! bearer URIs exist only in the response written to the requesting stream and
//! every `Debug` implementation redacts them.

use std::collections::BTreeSet;
use std::sync::Arc;

#[cfg(test)]
use collab_types::FilePermission;
use collab_types::{Ed25519PublicKey, FileRecordId, SHARE_CONTROL_FRAMING};
pub use collab_types::{
    MAX_SHARE_CONTROL_FRAME_BYTES, NamedGrantMetadataV1, NamedShareDeliveryStateV1,
    SHARE_CONTROL_VERSION, ShareControlErrorCode, ShareControlOperationV1, ShareControlRequestV1,
    ShareControlResponseV1, ShareControlResultV1,
};
use iroh_base::EndpointAddr;
use serde::Serialize;
use thiserror::Error;
use tilo_clock::Clock;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::canonical_frame::{self, CanonicalFrameError};
use crate::clock::DaemonClock;
use crate::record_repository::{RecordRepository, RecordRepositoryKey};
use crate::share_management::{
    BearerLinkRequest, MAX_TRUSTED_SHARE_ISSUERS, NamedGrantRequest, ShareManagement,
    ShareManagementError,
};

pub const MAX_SHARE_CONTROL_CONNECTIONS: usize = 32;
pub const MAX_SHARE_CONTROL_REQUESTS_PER_CONNECTION: usize = 256;

/// Immutable authority context for one workspace's Files control socket.
pub struct ShareControlServer {
    workspace_id: Uuid,
    endpoint: EndpointAddr,
    issuer: ed25519_dalek::SigningKey,
    trusted_issuers: Vec<Ed25519PublicKey>,
    management: Arc<ShareManagement>,
    repository: Arc<RecordRepository>,
    clock: Arc<dyn Clock>,
    named_outbox: Option<Arc<crate::NamedShareOutbox>>,
}

impl ShareControlServer {
    /// Construct a server using the process security clock.
    ///
    /// # Errors
    ///
    /// Returns an error when workspace, endpoint, issuer set, or issuer
    /// membership is invalid.
    pub fn new(
        workspace_id: Uuid,
        endpoint: EndpointAddr,
        issuer: ed25519_dalek::SigningKey,
        trusted_issuers: Vec<Ed25519PublicKey>,
        management: Arc<ShareManagement>,
        repository: Arc<RecordRepository>,
    ) -> Result<Self, ShareControlServerError> {
        Self::with_clock(
            workspace_id,
            endpoint,
            issuer,
            trusted_issuers,
            management,
            repository,
            Arc::new(DaemonClock),
        )
    }

    /// Construct a server with an injected clock.
    ///
    /// # Errors
    ///
    /// Returns an error for the same invalid authority configuration as
    /// [`Self::new`].
    pub fn with_clock(
        workspace_id: Uuid,
        endpoint: EndpointAddr,
        issuer: ed25519_dalek::SigningKey,
        trusted_issuers: Vec<Ed25519PublicKey>,
        management: Arc<ShareManagement>,
        repository: Arc<RecordRepository>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ShareControlServerError> {
        if workspace_id.is_nil() || endpoint.is_empty() {
            return Err(ShareControlServerError::InvalidConfiguration);
        }
        if trusted_issuers.is_empty() || trusted_issuers.len() > MAX_TRUSTED_SHARE_ISSUERS {
            return Err(ShareControlServerError::InvalidConfiguration);
        }
        let mut unique = BTreeSet::new();
        for key in &trusted_issuers {
            ed25519_dalek::VerifyingKey::from_bytes(key.as_bytes())
                .map_err(|_| ShareControlServerError::InvalidConfiguration)?;
            if !unique.insert(*key) {
                return Err(ShareControlServerError::InvalidConfiguration);
            }
        }
        let issuer_public = Ed25519PublicKey::from_bytes(issuer.verifying_key().to_bytes());
        if !unique.contains(&issuer_public) {
            return Err(ShareControlServerError::InvalidConfiguration);
        }
        Ok(Self {
            workspace_id,
            endpoint,
            issuer,
            trusted_issuers,
            management,
            repository,
            clock,
            named_outbox: None,
        })
    }

    #[must_use]
    pub fn with_named_delivery_outbox(mut self, outbox: Arc<crate::NamedShareOutbox>) -> Self {
        self.named_outbox = Some(outbox);
        self
    }

    /// Serve accepted streams with a hard global connection bound. Acquiring
    /// a permit before `accept` prevents an accept-loop task flood.
    ///
    /// # Errors
    ///
    /// Returns an error when permit acquisition, socket acceptance, or the
    /// listener itself fails.
    pub async fn serve(self: Arc<Self>, listener: UnixListener) -> std::io::Result<()> {
        let slots = Arc::new(Semaphore::new(MAX_SHARE_CONTROL_CONNECTIONS));
        loop {
            let permit = slots
                .clone()
                .acquire_owned()
                .await
                .map_err(std::io::Error::other)?;
            let (stream, _) = listener.accept().await?;
            let server = self.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let _ = server.handle_connection(stream).await;
            });
        }
    }

    async fn handle_connection(self: Arc<Self>, mut stream: UnixStream) -> std::io::Result<()> {
        for _ in 0..MAX_SHARE_CONTROL_REQUESTS_PER_CONNECTION {
            let request = match canonical_frame::read::<_, ShareControlRequestV1>(
                SHARE_CONTROL_FRAMING,
                &mut stream,
            )
            .await
            {
                Ok(Some(request)) => request,
                Ok(None) => return Ok(()),
                Err(CanonicalFrameError::TooLarge(_)) => {
                    let response = error_response(ShareControlErrorCode::FrameTooLarge);
                    let _ = write_frame(&mut stream, &response).await;
                    return Ok(());
                }
                Err(CanonicalFrameError::Io(error)) => return Err(error),
                Err(
                    CanonicalFrameError::Empty
                    | CanonicalFrameError::Decode(_)
                    | CanonicalFrameError::Encode(_),
                ) => {
                    let response = error_response(ShareControlErrorCode::InvalidRequest);
                    let _ = write_frame(&mut stream, &response).await;
                    return Ok(());
                }
            };
            let server = self.clone();
            let response = tokio::task::spawn_blocking(move || server.apply(request))
                .await
                .map_err(std::io::Error::other)?;
            write_frame(&mut stream, &response).await?;
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive wire-operation dispatcher keeps protocol validation and response mapping auditable in one place"
    )]
    fn apply(&self, request: ShareControlRequestV1) -> ShareControlResponseV1 {
        if request.protocol_version != SHARE_CONTROL_VERSION {
            return error_response(ShareControlErrorCode::UnsupportedVersion);
        }
        let now_ms = self.clock.now_ms();
        let result = match request.operation {
            ShareControlOperationV1::CreateNamed {
                record_id,
                person,
                recipient_devices,
                permission,
                expires_at_ms,
            } => {
                if self.record_exists(record_id) {
                    match self.management.create_named(
                        NamedGrantRequest {
                            workspace_id: self.workspace_id,
                            record_id,
                            recipient: person,
                            permission,
                            expires_at_ms,
                        },
                        &self.issuer,
                        &self.trusted_issuers,
                        now_ms,
                    ) {
                        Ok(token) => {
                            let queued = match self.named_outbox.as_ref() {
                                Some(outbox) => crate::NamedShareEnvelopeV1::bind_at(
                                    self.endpoint.clone(),
                                    token.clone(),
                                    &self.issuer,
                                    now_ms,
                                )
                                .and_then(|envelope| outbox.enqueue(envelope, recipient_devices)),
                                None => Err(crate::NamedShareInboxError::NoRecipientDevices),
                            };
                            queued.map_or_else(
                                |_| {
                                    let _ = self.management.revoke(token.id);
                                    ShareControlResultV1::Error {
                                        code: ShareControlErrorCode::InvalidRequest,
                                    }
                                },
                                |_| {
                                    ShareControlResultV1::NamedCreated(NamedGrantMetadataV1 {
                                        grant_id: token.id,
                                        workspace_id: token.workspace_id,
                                        record_id: token.record_id,
                                        person,
                                        permission: token.permission,
                                        expires_at_ms: token.expires_at_ms,
                                        issuer: token.issuer,
                                        delivery: NamedShareDeliveryStateV1::Pending,
                                    })
                                },
                            )
                        }
                        Err(error) => management_error(&error),
                    }
                } else {
                    ShareControlResultV1::Error {
                        code: ShareControlErrorCode::RecordNotFound,
                    }
                }
            }
            ShareControlOperationV1::CreateBearerV2 {
                record_id,
                permission,
                expires_at_ms,
                max_uses,
            } => {
                if self.record_exists(record_id) {
                    match self.management.create_bearer_v2(
                        BearerLinkRequest {
                            workspace_id: self.workspace_id,
                            record_id,
                            endpoint: self.endpoint.clone(),
                            permission,
                            expires_at_ms,
                            max_redemptions: max_uses.unwrap_or(1),
                        },
                        &self.issuer,
                        &self.trusted_issuers,
                        now_ms,
                    ) {
                        Ok(link) => match link.encode_uri_at(now_ms) {
                            Ok(uri) => ShareControlResultV1::BearerCreatedV2 { uri },
                            Err(_) => ShareControlResultV1::Error {
                                code: ShareControlErrorCode::Internal,
                            },
                        },
                        Err(error) => management_error(&error),
                    }
                } else {
                    ShareControlResultV1::Error {
                        code: ShareControlErrorCode::RecordNotFound,
                    }
                }
            }
            ShareControlOperationV1::Revoke { grant_id } => {
                match self.management.revoke(grant_id) {
                    Ok(revoked) => {
                        if revoked && let Some(outbox) = &self.named_outbox {
                            let _ = outbox.cancel(grant_id);
                        }
                        ShareControlResultV1::Revoked { grant_id, revoked }
                    }
                    Err(error) => management_error(&error),
                }
            }
        };
        ShareControlResponseV1 {
            protocol_version: SHARE_CONTROL_VERSION,
            result,
        }
    }

    fn record_exists(&self, record_id: FileRecordId) -> bool {
        self.repository.contains(RecordRepositoryKey {
            workspace_id: self.workspace_id,
            record_id,
        })
    }
}

fn management_error(error: &ShareManagementError) -> ShareControlResultV1 {
    use crate::share_authority::ShareAuthorityError;
    let code = match error {
        ShareManagementError::Authority(ShareAuthorityError::TooManyRecordGrants) => {
            ShareControlErrorCode::LimitReached
        }
        ShareManagementError::Authority(_) => ShareControlErrorCode::Internal,
        ShareManagementError::Invite(_)
        | ShareManagementError::Link(_)
        | ShareManagementError::InvalidScope
        | ShareManagementError::InvalidIdentityKey
        | ShareManagementError::InvalidTrustedIssuerSet
        | ShareManagementError::UntrustedIssuer
        | ShareManagementError::ExpiryNotFuture
        | ShareManagementError::ExpiryRequired
        | ShareManagementError::ExpiryTooDistant
        | ShareManagementError::InvalidRedemptionLimit => ShareControlErrorCode::InvalidRequest,
    };
    ShareControlResultV1::Error { code }
}

fn error_response(code: ShareControlErrorCode) -> ShareControlResponseV1 {
    ShareControlResponseV1 {
        protocol_version: SHARE_CONTROL_VERSION,
        result: ShareControlResultV1::Error { code },
    }
}

async fn write_frame<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize + Sync,
{
    canonical_frame::write(SHARE_CONTROL_FRAMING, writer, value)
        .await
        .map_err(|error| match error {
            CanonicalFrameError::Io(error) => error,
            CanonicalFrameError::Encode(error) => std::io::Error::other(error),
            CanonicalFrameError::Empty
            | CanonicalFrameError::TooLarge(_)
            | CanonicalFrameError::Decode(_) => {
                std::io::Error::other("share control response too large")
            }
        })?;
    writer.flush().await
}

/// Write one bounded canonical request frame.
///
/// # Errors
///
/// Returns an error when encoding or socket I/O fails or the request exceeds
/// the protocol frame bound.
pub async fn write_share_control_request(
    stream: &mut UnixStream,
    request: &ShareControlRequestV1,
) -> std::io::Result<()> {
    write_frame(stream, request).await
}

/// Read one bounded canonical response frame.
///
/// # Errors
///
/// Returns an error for EOF, malformed/oversized responses, or socket I/O.
pub async fn read_share_control_response(
    stream: &mut UnixStream,
) -> std::io::Result<ShareControlResponseV1> {
    canonical_frame::read(SHARE_CONTROL_FRAMING, stream)
        .await
        .map_err(|error| match error {
            CanonicalFrameError::Io(error) => error,
            CanonicalFrameError::TooLarge(_) => {
                std::io::Error::other("share control response too large")
            }
            CanonicalFrameError::Empty => {
                std::io::Error::other("invalid share control response: zero-length frame")
            }
            CanonicalFrameError::Decode(error) | CanonicalFrameError::Encode(error) => {
                std::io::Error::other(format!("invalid share control response: {error}"))
            }
        })?
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
}

#[derive(Debug, Error)]
pub enum ShareControlServerError {
    #[error("share control server configuration is invalid")]
    InvalidConfiguration,
}

#[cfg(test)]
mod tests {
    use collab_types::{ContentHash, FileActor, FileMetadataV1, IndexStatus, generate_keypair};
    use iroh::SecretKey;

    use tilo_clock::FixedClock;

    use super::*;
    use crate::record_publisher::RecordPublisher;
    use crate::share_authority::{ShareAuthority, ShareGrantStatus};

    const NOW: u64 = 20_000;

    struct Fixture {
        _dir: tempfile::TempDir,
        server: Arc<ShareControlServer>,
        authority: Arc<ShareAuthority>,
        workspace_id: Uuid,
        record_id: FileRecordId,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let repository = Arc::new(RecordRepository::open(dir.path().join("records")).unwrap());
            let workspace_id = Uuid::from_u128(201);
            let record_id = FileRecordId(Uuid::from_u128(202));
            let (device_key, device) = generate_keypair();
            let (_, person) = generate_keypair();
            let actor = FileActor { person, device };
            let publisher =
                RecordPublisher::new(repository.clone(), workspace_id, actor, device_key).unwrap();
            publisher
                .publish(
                    &FileMetadataV1 {
                        schema_version: 1,
                        record_id,
                        content_hash: Some(ContentHash([7; 32])),
                        byte_len: 7,
                        mime: Some("text/plain".into()),
                        previews: Vec::new(),
                        latest_actor: Some(actor),
                        observed_at_ms: 1,
                        index_status: IndexStatus::Indexed,
                        properties: None,
                    },
                    &[],
                )
                .unwrap();
            let authority =
                Arc::new(ShareAuthority::open(dir.path().join("share-authority.json")).unwrap());
            let management = Arc::new(ShareManagement::new(authority.clone()));
            let issuer = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
            let issuer_public = Ed25519PublicKey::from_bytes(issuer.verifying_key().to_bytes());
            let endpoint = EndpointAddr::new(SecretKey::from_bytes(&[72; 32]).public())
                .with_ip_addr("127.0.0.1:7788".parse().unwrap());
            let server = Arc::new(
                ShareControlServer::with_clock(
                    workspace_id,
                    endpoint,
                    issuer,
                    vec![issuer_public],
                    management,
                    repository,
                    Arc::new(FixedClock(NOW)),
                )
                .unwrap()
                .with_named_delivery_outbox(Arc::new(
                    crate::NamedShareOutbox::open(dir.path().join("named-outbox.json")).unwrap(),
                )),
            );
            Self {
                _dir: dir,
                server,
                authority,
                workspace_id,
                record_id,
            }
        }

        async fn transact(&self, operation: ShareControlOperationV1) -> ShareControlResponseV1 {
            let (mut client, server_stream) = UnixStream::pair().unwrap();
            let server = self.server.clone();
            let task = tokio::spawn(async move { server.handle_connection(server_stream).await });
            write_share_control_request(
                &mut client,
                &ShareControlRequestV1 {
                    protocol_version: SHARE_CONTROL_VERSION,
                    operation,
                },
            )
            .await
            .unwrap();
            let response = read_share_control_response(&mut client).await.unwrap();
            drop(client);
            task.await.unwrap().unwrap();
            response
        }
    }

    #[tokio::test]
    async fn frames_are_big_endian_length_then_canonical_bytes() {
        let response = error_response(ShareControlErrorCode::FrameTooLarge);
        let mut wire = Vec::new();
        write_frame(&mut wire, &response).await.unwrap();
        assert_eq!(wire, [0, 0, 0, 3, 2, 3, 4]);

        let mut wire: Vec<u8> = vec![0, 0, 0, 19, 2, 2, 16];
        wire.extend_from_slice(&[0x11; 16]);
        let request: ShareControlRequestV1 =
            canonical_frame::read(SHARE_CONTROL_FRAMING, &mut wire.as_slice())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            request,
            ShareControlRequestV1 {
                protocol_version: SHARE_CONTROL_VERSION,
                operation: ShareControlOperationV1::Revoke {
                    grant_id: Uuid::from_bytes([0x11; 16]),
                },
            }
        );
    }

    #[tokio::test]
    async fn empty_and_truncated_frames_end_the_connection() {
        for (wire, answer) in [
            // A zero-length frame is an invalid request.
            (
                &[0, 0, 0, 0][..],
                Some(ShareControlErrorCode::InvalidRequest),
            ),
            // A stream closed inside the prefix is a clean close.
            (&[0, 0][..], None),
        ] {
            let fixture = Fixture::new();
            let (mut client, server_stream) = UnixStream::pair().unwrap();
            let server = fixture.server.clone();
            let task = tokio::spawn(async move { server.handle_connection(server_stream).await });
            client.write_all(wire).await.unwrap();
            client.shutdown().await.unwrap();
            let response = read_share_control_response(&mut client).await;
            match answer {
                Some(code) => assert_eq!(
                    response.unwrap().result,
                    ShareControlResultV1::Error { code }
                ),
                None => assert_eq!(
                    response.unwrap_err().kind(),
                    std::io::ErrorKind::UnexpectedEof
                ),
            }
            task.await.unwrap().unwrap();
        }

        // A stream closed inside the payload is an I/O error.
        let error = read_share_control_response_from(&[0, 0, 0, 3, 2]).await;
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    async fn read_share_control_response_from(wire: &[u8]) -> std::io::Error {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        server.write_all(wire).await.unwrap();
        drop(server);
        read_share_control_response(&mut client).await.unwrap_err()
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn share_control_creates_and_revokes_only_known_records_with_redacted_bounded_frames() {
        // Phase `creates_named_and_bearer_then_revokes_over_requesting_stream`.
        {
            let fixture = Fixture::new();
            let (_, recipient) = generate_keypair();
            let named = fixture
                .transact(ShareControlOperationV1::CreateNamed {
                    record_id: fixture.record_id,
                    person: recipient,
                    recipient_devices: vec![recipient],
                    permission: FilePermission::Edit,
                    expires_at_ms: Some(NOW + 1_000),
                })
                .await;
            let grant_id = match named.result {
                ShareControlResultV1::NamedCreated(metadata) => {
                    assert_eq!(metadata.workspace_id, fixture.workspace_id);
                    assert_eq!(metadata.person, recipient);
                    metadata.grant_id
                }
                other => panic!("unexpected response: {other:?}"),
            };

            let bearer = fixture
                .transact(ShareControlOperationV1::CreateBearerV2 {
                    record_id: fixture.record_id,
                    permission: FilePermission::View,
                    expires_at_ms: NOW + 1_000,
                    max_uses: None,
                })
                .await;
            let uri = match bearer.result {
                ShareControlResultV1::BearerCreatedV2 { uri } => uri,
                other => panic!("unexpected response: {other:?}"),
            };
            let decoded = crate::FileShareLinkV2::decode_uri_at(&uri, NOW).unwrap();
            assert_eq!(decoded.invite.record_id, fixture.record_id);
            assert_eq!(decoded.invite.max_redemptions, Some(1));

            let revoked = fixture
                .transact(ShareControlOperationV1::Revoke { grant_id })
                .await;
            assert_eq!(
                revoked.result,
                ShareControlResultV1::Revoked {
                    grant_id,
                    revoked: true
                }
            );
            assert_eq!(
                fixture
                    .authority
                    .projection(fixture.record_id, NOW)
                    .grants
                    .iter()
                    .find(|grant| grant.grant_id == grant_id)
                    .unwrap()
                    .status,
                ShareGrantStatus::Revoked
            );
        }
        // Phase `unknown_record_cannot_create_any_authority_state`.
        {
            let fixture = Fixture::new();
            let (_, recipient) = generate_keypair();
            let response = fixture
                .transact(ShareControlOperationV1::CreateNamed {
                    record_id: FileRecordId::new(),
                    person: recipient,
                    recipient_devices: vec![recipient],
                    permission: FilePermission::View,
                    expires_at_ms: None,
                })
                .await;
            assert_eq!(
                response.result,
                ShareControlResultV1::Error {
                    code: ShareControlErrorCode::RecordNotFound
                }
            );
            assert_eq!(
                fixture
                    .authority
                    .projection(fixture.record_id, NOW)
                    .grants
                    .len(),
                0
            );
        }
        // Phase `oversized_frame_is_rejected_before_payload_allocation`.
        {
            let fixture = Fixture::new();
            let (mut client, server_stream) = UnixStream::pair().unwrap();
            let server = fixture.server.clone();
            let task = tokio::spawn(async move { server.handle_connection(server_stream).await });
            client
                .write_all(
                    &u32::try_from(MAX_SHARE_CONTROL_FRAME_BYTES + 1)
                        .unwrap()
                        .to_be_bytes(),
                )
                .await
                .unwrap();
            let response = read_share_control_response(&mut client).await.unwrap();
            assert_eq!(
                response.result,
                ShareControlResultV1::Error {
                    code: ShareControlErrorCode::FrameTooLarge
                }
            );
            task.await.unwrap().unwrap();
        }
        // Phase `bearer_uri_is_redacted_from_every_response_debug_path`.
        {
            let fixture = Fixture::new();
            let response = fixture
                .transact(ShareControlOperationV1::CreateBearerV2 {
                    record_id: fixture.record_id,
                    permission: FilePermission::View,
                    expires_at_ms: NOW + 1_000,
                    max_uses: Some(2),
                })
                .await;
            let uri = match &response.result {
                ShareControlResultV1::BearerCreatedV2 { uri } => uri,
                other => panic!("unexpected response: {other:?}"),
            };
            let fragment = uri.split_once('#').unwrap().1;
            let debug = format!("{response:?}");
            assert!(!debug.contains(uri));
            assert!(!debug.contains(fragment));
            assert!(debug.contains("<redacted>"));
        }
    }
}
