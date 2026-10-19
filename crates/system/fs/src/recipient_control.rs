//! Owner-local admission and status protocol for recipient file shares.
//!
//! Bearer V2 URIs are accepted only inside bounded stream frames. They are
//! never accepted as command-line input and every request Debug path redacts
//! them. Responses contain only record keys and coarse subscription status.

use std::path::Path;
use std::sync::Arc;

use fs_protocol::{
    MAX_RECIPIENT_CONTROL_CONNECTIONS, MAX_RECIPIENT_CONTROL_REQUESTS_PER_CONNECTION,
    MAX_RECIPIENT_MOUNT_PATH_BYTES, RECIPIENT_CONTROL_FRAMING, RECIPIENT_CONTROL_VERSION,
    RecipientControlErrorCode, RecipientControlOperationV1, RecipientControlRequestV1,
    RecipientControlResponseV1, RecipientControlResultV1, RecipientFailureCodeV1,
    RecipientFailureV1, RecipientMountStatusV1, RecipientSubscriptionKeyV1,
    RecipientSubscriptionStatusV1,
};
use serde::Serialize;
use thiserror::Error;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;

use crate::RecipientMountSupervisor;
use crate::canonical_frame::{self, CanonicalFrameError};
use crate::record_subscription::{
    MAX_RECORD_SUBSCRIPTION_URI_BYTES, RecordSubscription, RecordSubscriptionFailure,
    RecordSubscriptionKey, RecordSubscriptionStore, RecordSubscriptionStoreError,
};
use crate::share_link::FileShareLinkV2;

impl From<RecordSubscription> for RecipientSubscriptionStatusV1 {
    fn from(subscription: RecordSubscription) -> Self {
        Self {
            key: subscription.key.into(),
            permission: subscription.permission,
            last_success_at_ms: subscription.last_success_at_ms,
            last_error: subscription.last_error.map(Into::into),
            mount: None,
        }
    }
}

impl From<RecordSubscriptionKey> for RecipientSubscriptionKeyV1 {
    fn from(key: RecordSubscriptionKey) -> Self {
        Self {
            workspace_id: key.workspace_id,
            record_id: key.record_id,
        }
    }
}

impl From<RecipientSubscriptionKeyV1> for RecordSubscriptionKey {
    fn from(key: RecipientSubscriptionKeyV1) -> Self {
        Self {
            workspace_id: key.workspace_id,
            record_id: key.record_id,
        }
    }
}

impl From<RecordSubscriptionFailure> for RecipientFailureV1 {
    fn from(failure: RecordSubscriptionFailure) -> Self {
        let code = match failure.code {
            crate::RecordSubscriptionFailureCode::Offline => RecipientFailureCodeV1::Offline,
            crate::RecordSubscriptionFailureCode::Authentication => {
                RecipientFailureCodeV1::Authentication
            }
            crate::RecordSubscriptionFailureCode::Denied => RecipientFailureCodeV1::Denied,
            crate::RecordSubscriptionFailureCode::NotFound => RecipientFailureCodeV1::NotFound,
            crate::RecordSubscriptionFailureCode::InvalidResponse => {
                RecipientFailureCodeV1::InvalidResponse
            }
            crate::RecordSubscriptionFailureCode::Internal => RecipientFailureCodeV1::Internal,
        };
        Self {
            code,
            observed_at_ms: failure.observed_at_ms,
        }
    }
}

#[derive(Debug)]
pub struct RecipientControlServer {
    subscriptions: Arc<RecordSubscriptionStore>,
    mounts: Option<Arc<RecipientMountSupervisor>>,
}

impl RecipientControlServer {
    /// Construct a server that manages persisted recipient subscriptions.
    pub fn new(subscriptions: Arc<RecordSubscriptionStore>) -> Self {
        Self {
            subscriptions,
            mounts: None,
        }
    }

    /// Construct a server that also supervises recipient mounts.
    pub fn with_mounts(
        subscriptions: Arc<RecordSubscriptionStore>,
        mounts: Arc<RecipientMountSupervisor>,
    ) -> Self {
        Self {
            subscriptions,
            mounts: Some(mounts),
        }
    }

    /// Bind a new owner-only Unix socket. Existing paths are never removed or
    /// replaced implicitly.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot be bound or secured to mode
    /// `0600`.
    pub fn bind(path: impl AsRef<Path>) -> std::io::Result<UnixListener> {
        let path = path.as_ref();
        let listener = UnixListener::bind(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
            if mode != 0o600 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "recipient control socket is not owner-only",
                ));
            }
        }
        Ok(listener)
    }

    /// Serve streams with a hard global task bound. A permit is acquired
    /// before accept, so overload cannot create an unbounded task queue.
    ///
    /// # Errors
    ///
    /// Returns an error when accepting a connection or acquiring a task slot
    /// fails.
    pub async fn serve(self: Arc<Self>, listener: UnixListener) -> std::io::Result<()> {
        let slots = Arc::new(Semaphore::new(MAX_RECIPIENT_CONTROL_CONNECTIONS));
        loop {
            let permit = slots
                .clone()
                .acquire_owned()
                .await
                .map_err(std::io::Error::other)?;
            let (stream, _) = listener.accept().await?;
            let server = Arc::clone(&self);
            tokio::spawn(async move {
                let _permit = permit;
                let _ = server.handle_connection(stream).await;
            });
        }
    }

    async fn handle_connection(self: Arc<Self>, mut stream: UnixStream) -> std::io::Result<()> {
        for _ in 0..MAX_RECIPIENT_CONTROL_REQUESTS_PER_CONNECTION {
            let request = match canonical_frame::read::<_, RecipientControlRequestV1>(
                RECIPIENT_CONTROL_FRAMING,
                &mut stream,
            )
            .await
            {
                Ok(Some(request)) => request,
                Ok(None) => return Ok(()),
                Err(CanonicalFrameError::TooLarge(len)) => {
                    tracing::debug!(len, "refused a recipient control frame over the limit");
                    let _ = write_frame(
                        &mut stream,
                        &error_response(RecipientControlErrorCode::FrameTooLarge),
                    )
                    .await;
                    return Ok(());
                }
                Err(
                    CanonicalFrameError::Empty
                    | CanonicalFrameError::Decode(_)
                    | CanonicalFrameError::Encode(_),
                ) => {
                    let _ = write_frame(
                        &mut stream,
                        &error_response(RecipientControlErrorCode::InvalidRequest),
                    )
                    .await;
                    return Ok(());
                }
                Err(CanonicalFrameError::Io(error)) => return Err(error),
            };
            let server = Arc::clone(&self);
            let response = tokio::task::spawn_blocking(move || server.apply(request))
                .await
                .map_err(std::io::Error::other)?;
            write_frame(&mut stream, &response).await?;
        }
        Ok(())
    }

    fn apply(&self, request: RecipientControlRequestV1) -> RecipientControlResponseV1 {
        if request.protocol_version != RECIPIENT_CONTROL_VERSION {
            return error_response(RecipientControlErrorCode::UnsupportedVersion);
        }
        let result = match request.operation {
            RecipientControlOperationV1::AdmitV2 { uri } => {
                if uri.len() > MAX_RECORD_SUBSCRIPTION_URI_BYTES {
                    RecipientControlResultV1::Error {
                        code: RecipientControlErrorCode::InvalidLink,
                    }
                } else {
                    match FileShareLinkV2::decode_uri(&uri) {
                        Ok(link) => {
                            let key = RecordSubscriptionKey {
                                workspace_id: link.invite.workspace_id,
                                record_id: link.invite.record_id,
                            };
                            match self.subscriptions.insert(link) {
                                Ok(inserted) => match self.subscriptions.get(key) {
                                    Some(subscription) => match self.start_and_status(subscription)
                                    {
                                        Ok(status) => {
                                            RecipientControlResultV1::Admitted { inserted, status }
                                        }
                                        Err(code) => RecipientControlResultV1::Error { code },
                                    },
                                    None => RecipientControlResultV1::Error {
                                        code: RecipientControlErrorCode::Internal,
                                    },
                                },
                                Err(error) => store_error(&error),
                            }
                        }
                        Err(_) => RecipientControlResultV1::Error {
                            code: RecipientControlErrorCode::InvalidLink,
                        },
                    }
                }
            }
            RecipientControlOperationV1::Status { key } => match self.subscriptions.get(key.into())
            {
                Some(subscription) => match self.status(subscription) {
                    Ok(status) => RecipientControlResultV1::Status(status),
                    Err(code) => RecipientControlResultV1::Error { code },
                },
                None => RecipientControlResultV1::Error {
                    code: RecipientControlErrorCode::NotFound,
                },
            },
            RecipientControlOperationV1::Retry { key } => {
                match self.subscriptions.get(key.into()) {
                    Some(subscription) => match self.start_and_status(subscription) {
                        Ok(status) => RecipientControlResultV1::Status(status),
                        Err(code) => RecipientControlResultV1::Error { code },
                    },
                    None => RecipientControlResultV1::Error {
                        code: RecipientControlErrorCode::NotFound,
                    },
                }
            }
        };
        RecipientControlResponseV1 {
            protocol_version: RECIPIENT_CONTROL_VERSION,
            result,
        }
    }

    fn start_and_status(
        &self,
        subscription: RecordSubscription,
    ) -> Result<RecipientSubscriptionStatusV1, RecipientControlErrorCode> {
        if let Some(mounts) = &self.mounts {
            mounts
                .start(&subscription)
                .map_err(|_| RecipientControlErrorCode::Internal)?;
        }
        self.status(subscription)
    }

    fn status(
        &self,
        subscription: RecordSubscription,
    ) -> Result<RecipientSubscriptionStatusV1, RecipientControlErrorCode> {
        let key = subscription.key;
        let mut status: RecipientSubscriptionStatusV1 = subscription.into();
        if let Some(mounts) = &self.mounts {
            let path = mounts.mount_point_for(key);
            let path = path
                .to_str()
                .filter(|path| path.len() <= MAX_RECIPIENT_MOUNT_PATH_BYTES)
                .ok_or(RecipientControlErrorCode::Internal)?;
            status.mount = Some(RecipientMountStatusV1 {
                path: path.to_owned(),
                running: mounts
                    .is_running(key)
                    .map_err(|_| RecipientControlErrorCode::Internal)?,
            });
        }
        Ok(status)
    }
}

fn store_error(error: &RecordSubscriptionStoreError) -> RecipientControlResultV1 {
    let code = match error {
        RecordSubscriptionStoreError::InvalidLink
        | RecordSubscriptionStoreError::NonCanonicalState
        | RecordSubscriptionStoreError::KeyMismatch => RecipientControlErrorCode::InvalidLink,
        RecordSubscriptionStoreError::DuplicateCollision(_) => RecipientControlErrorCode::Collision,
        RecordSubscriptionStoreError::CapacityReached
        | RecordSubscriptionStoreError::StateTooLarge => RecipientControlErrorCode::LimitReached,
        RecordSubscriptionStoreError::UnknownSubscription(_) => RecipientControlErrorCode::NotFound,
        _ => RecipientControlErrorCode::Internal,
    };
    RecipientControlResultV1::Error { code }
}

fn error_response(code: RecipientControlErrorCode) -> RecipientControlResponseV1 {
    RecipientControlResponseV1 {
        protocol_version: RECIPIENT_CONTROL_VERSION,
        result: RecipientControlResultV1::Error { code },
    }
}

async fn write_frame<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize + Sync,
{
    canonical_frame::write(RECIPIENT_CONTROL_FRAMING, writer, value)
        .await
        .map_err(|error| match error {
            CanonicalFrameError::Io(error) => error,
            CanonicalFrameError::Encode(error) => std::io::Error::other(error),
            CanonicalFrameError::Empty
            | CanonicalFrameError::TooLarge(_)
            | CanonicalFrameError::Decode(_) => {
                std::io::Error::other("recipient control response exceeds its bound")
            }
        })?;
    writer.flush().await
}

/// Write one bounded recipient-control request frame.
///
/// # Errors
///
/// Returns an error when encoding or writing the frame fails.
pub async fn write_recipient_control_request(
    stream: &mut UnixStream,
    request: &RecipientControlRequestV1,
) -> std::io::Result<()> {
    write_frame(stream, request).await
}

/// Read one bounded recipient-control response frame.
///
/// # Errors
///
/// Returns an error for I/O, malformed, oversized, or unexpectedly absent
/// response data.
pub async fn read_recipient_control_response(
    stream: &mut UnixStream,
) -> std::io::Result<RecipientControlResponseV1> {
    canonical_frame::read(RECIPIENT_CONTROL_FRAMING, stream)
        .await
        .map_err(|error| match error {
            CanonicalFrameError::Io(error) => error,
            CanonicalFrameError::TooLarge(_) => {
                std::io::Error::other("recipient control response too large")
            }
            CanonicalFrameError::Empty
            | CanonicalFrameError::Decode(_)
            | CanonicalFrameError::Encode(_) => {
                std::io::Error::other("invalid recipient control response")
            }
        })?
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::UnexpectedEof))
}

/// One-shot client helper. Secret-bearing requests are never formatted by this
/// API and live only in the caller-owned request and stream frame.
///
/// # Errors
///
/// Returns an error when connecting, writing the request, or reading the
/// response fails.
pub async fn request_recipient_control(
    socket: impl AsRef<Path>,
    request: &RecipientControlRequestV1,
) -> std::io::Result<RecipientControlResponseV1> {
    let mut stream = UnixStream::connect(socket).await?;
    write_recipient_control_request(&mut stream, request).await?;
    read_recipient_control_response(&mut stream).await
}

#[derive(Debug, Error)]
pub enum RecipientControlServerError {
    #[error("recipient control server failed")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_protocol::{
        FileInviteSpec, FileInviteSubject, FilePermission, FileRecordId,
        MAX_RECIPIENT_CONTROL_FRAME_BYTES, mint_bearer_file_invite,
    };
    use rand::{SeedableRng, rngs::StdRng};
    use uuid::Uuid;

    fn uri(seed: u8, workspace_id: Uuid, record_id: FileRecordId) -> String {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut rng = StdRng::seed_from_u64(seed.into());
        let bearer = mint_bearer_file_invite(
            FileInviteSpec {
                workspace_id,
                record_id,
                subject: FileInviteSubject::Bearer {
                    secret_hash: [0; 32],
                },
                permission: FilePermission::View,
                expires_at_ms: None,
                max_redemptions: None,
            },
            &issuer,
            &mut rng,
        )
        .unwrap();
        let endpoint = iroh_base::EndpointAddr::new(
            iroh::SecretKey::from_bytes(&[seed.wrapping_add(50); 32]).public(),
        )
        .with_ip_addr("127.0.0.1:4455".parse().unwrap());
        FileShareLinkV2::bind_at(endpoint, bearer, &issuer, tilo_clock::unix_now_ms())
            .unwrap()
            .encode_uri()
            .unwrap()
    }

    async fn transact(
        server: Arc<RecipientControlServer>,
        request: RecipientControlRequestV1,
    ) -> RecipientControlResponseV1 {
        let (mut client, server_stream) = UnixStream::pair().unwrap();
        let task = tokio::spawn(Arc::clone(&server).handle_connection(server_stream));
        write_recipient_control_request(&mut client, &request)
            .await
            .unwrap();
        let response = read_recipient_control_response(&mut client).await.unwrap();
        drop(client);
        task.await.unwrap().unwrap();
        response
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn recipient_control_admits_v2_redacts_status_and_bounds_frames() {
        // Phase `admits_v2_and_returns_only_redacted_status`.
        {
            let temp = tempfile::tempdir().unwrap();
            let store = Arc::new(
                RecordSubscriptionStore::open(temp.path().join("subscriptions.json")).unwrap(),
            );
            let server = Arc::new(RecipientControlServer::new(Arc::clone(&store)));
            let workspace_id = Uuid::new_v4();
            let record_id = FileRecordId::new();
            let secret_uri = uri(1, workspace_id, record_id);
            let secret = secret_uri.split_once('#').unwrap().1.to_string();
            let request = RecipientControlRequestV1 {
                protocol_version: RECIPIENT_CONTROL_VERSION,
                operation: RecipientControlOperationV1::AdmitV2 {
                    uri: secret_uri.clone(),
                },
            };
            let request_debug = format!("{request:?}");
            assert!(!request_debug.contains(&secret_uri));
            assert!(!request_debug.contains(&secret));

            let response = transact(Arc::clone(&server), request).await;
            let key = RecordSubscriptionKey {
                workspace_id,
                record_id,
            };
            assert!(matches!(
                &response.result,
                RecipientControlResultV1::Admitted {
                    inserted: true,
                    status: RecipientSubscriptionStatusV1 { key: got, .. }
                } if *got == key.into()
            ));
            let response_debug = format!("{response:?}");
            assert!(!response_debug.contains(&secret_uri));
            assert!(!response_debug.contains(&secret));
            assert!(store.get(key).is_some());

            store
                .record_error(
                    key,
                    crate::record_subscription::RecordSubscriptionFailureCode::Offline,
                    10,
                )
                .unwrap();
            let status = transact(
                server,
                RecipientControlRequestV1 {
                    protocol_version: RECIPIENT_CONTROL_VERSION,
                    operation: RecipientControlOperationV1::Status { key: key.into() },
                },
            )
            .await;
            assert!(matches!(
                status.result,
                RecipientControlResultV1::Status(RecipientSubscriptionStatusV1 {
                    last_error: Some(_),
                    ..
                })
            ));
        }
        // Phase `invalid_collision_and_oversized_frames_are_finite`.
        {
            let temp = tempfile::tempdir().unwrap();
            let store = Arc::new(
                RecordSubscriptionStore::open(temp.path().join("subscriptions.json")).unwrap(),
            );
            let server = Arc::new(RecipientControlServer::new(store));
            let invalid = transact(
                Arc::clone(&server),
                RecipientControlRequestV1 {
                    protocol_version: RECIPIENT_CONTROL_VERSION,
                    operation: RecipientControlOperationV1::AdmitV2 {
                        uri: "tilo-file://v2/not-a-link#redacted".into(),
                    },
                },
            )
            .await;
            assert_eq!(
                invalid.result,
                RecipientControlResultV1::Error {
                    code: RecipientControlErrorCode::InvalidLink
                }
            );

            let workspace = Uuid::new_v4();
            let record = FileRecordId::new();
            for (seed, expected) in [(2, false), (3, true)] {
                let response = transact(
                    Arc::clone(&server),
                    RecipientControlRequestV1 {
                        protocol_version: RECIPIENT_CONTROL_VERSION,
                        operation: RecipientControlOperationV1::AdmitV2 {
                            uri: uri(seed, workspace, record),
                        },
                    },
                )
                .await;
                assert_eq!(
                    matches!(
                        response.result,
                        RecipientControlResultV1::Error {
                            code: RecipientControlErrorCode::Collision
                        }
                    ),
                    expected
                );
            }

            let (mut client, server_stream) = UnixStream::pair().unwrap();
            let task = tokio::spawn(Arc::clone(&server).handle_connection(server_stream));
            client
                .write_all(
                    &u32::try_from(MAX_RECIPIENT_CONTROL_FRAME_BYTES + 1)
                        .unwrap()
                        .to_be_bytes(),
                )
                .await
                .unwrap();
            assert_eq!(
                read_recipient_control_response(&mut client)
                    .await
                    .unwrap()
                    .result,
                RecipientControlResultV1::Error {
                    code: RecipientControlErrorCode::FrameTooLarge
                }
            );
            drop(client);
            task.await.unwrap().unwrap();

            let (mut client, server_stream) = UnixStream::pair().unwrap();
            let task = tokio::spawn(Arc::clone(&server).handle_connection(server_stream));
            let mut non_canonical = collab_types::canonical_bytes(&RecipientControlRequestV1 {
                protocol_version: RECIPIENT_CONTROL_VERSION,
                operation: RecipientControlOperationV1::Status {
                    key: RecordSubscriptionKey {
                        workspace_id: Uuid::new_v4(),
                        record_id: FileRecordId::new(),
                    }
                    .into(),
                },
            })
            .unwrap();
            non_canonical.push(0);
            client
                .write_all(&u32::try_from(non_canonical.len()).unwrap().to_be_bytes())
                .await
                .unwrap();
            client.write_all(&non_canonical).await.unwrap();
            assert_eq!(
                read_recipient_control_response(&mut client)
                    .await
                    .unwrap()
                    .result,
                RecipientControlResultV1::Error {
                    code: RecipientControlErrorCode::InvalidRequest
                }
            );
            drop(client);
            task.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn frames_are_big_endian_length_then_canonical_bytes() {
        let mut wire = Vec::new();
        write_frame(
            &mut wire,
            &error_response(RecipientControlErrorCode::FrameTooLarge),
        )
        .await
        .unwrap();
        assert_eq!(wire, [0, 0, 0, 3, 1, 2, 6]);

        let wire = [0, 0, 0, 5, 1, 0, 2, b'a', b'b'];
        let request: RecipientControlRequestV1 =
            canonical_frame::read(RECIPIENT_CONTROL_FRAMING, &mut &wire[..])
                .await
                .unwrap()
                .unwrap();
        assert_eq!(
            request,
            RecipientControlRequestV1 {
                protocol_version: RECIPIENT_CONTROL_VERSION,
                operation: RecipientControlOperationV1::AdmitV2 { uri: "ab".into() },
            }
        );
    }

    #[tokio::test]
    async fn client_reads_empty_truncated_and_closed_responses_as_errors() {
        for (wire, kind) in [
            (&[][..], std::io::ErrorKind::UnexpectedEof),
            // A stream closed inside the prefix is a close, not a frame.
            (&[0, 0][..], std::io::ErrorKind::UnexpectedEof),
            (&[0, 0, 0, 3, 1][..], std::io::ErrorKind::UnexpectedEof),
            (&[0, 0, 0, 0][..], std::io::ErrorKind::Other),
        ] {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            server.write_all(wire).await.unwrap();
            drop(server);
            let error = read_recipient_control_response(&mut client)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), kind, "{wire:?}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recipient_control_socket_is_private_and_admission_isolates_the_mount() {
        // Phase `bound_socket_is_mode_0600_and_one_shot_client_round_trips`.
        {
            use std::os::unix::fs::PermissionsExt;
            let temp = tempfile::tempdir().unwrap();
            let socket = temp.path().join("recipient.sock");
            let store = Arc::new(
                RecordSubscriptionStore::open(temp.path().join("subscriptions.json")).unwrap(),
            );
            let server = Arc::new(RecipientControlServer::new(store));
            let listener = RecipientControlServer::bind(&socket).unwrap();
            assert_eq!(
                std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let server_task = tokio::spawn(Arc::clone(&server).serve(listener));
            let response = request_recipient_control(
                &socket,
                &RecipientControlRequestV1 {
                    protocol_version: RECIPIENT_CONTROL_VERSION,
                    operation: RecipientControlOperationV1::Status {
                        key: RecordSubscriptionKey {
                            workspace_id: Uuid::new_v4(),
                            record_id: FileRecordId::new(),
                        }
                        .into(),
                    },
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                response.result,
                RecipientControlResultV1::Error {
                    code: RecipientControlErrorCode::NotFound
                }
            ));
            server_task.abort();
        }
        // Phase `admission_starts_an_isolated_mount_and_reports_only_its_public_path`.
        {
            use std::os::unix::fs::PermissionsExt;
            let temp = tempfile::tempdir().unwrap();
            let executable = temp.path().join("fake-fs");
            std::fs::write(
                &executable,
                format!(
                    "#!/bin/sh\n/bin/cat >/dev/null\nprintf '%s\\n' '{}'\nexec /bin/sleep 60\n",
                    crate::recipient_mount::RECIPIENT_MOUNT_READY_LINE
                ),
            )
            .unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let store = Arc::new(
                RecordSubscriptionStore::open(temp.path().join("subscriptions.json")).unwrap(),
            );
            let mounts = Arc::new(crate::RecipientMountSupervisor::new(
                &executable,
                temp.path().join("mounts"),
                temp.path().join("cache"),
            ));
            let server = Arc::new(RecipientControlServer::with_mounts(
                Arc::clone(&store),
                Arc::clone(&mounts),
            ));
            let workspace_id = Uuid::new_v4();
            let record_id = FileRecordId::new();
            let secret_uri = uri(9, workspace_id, record_id);
            let response = transact(
                server,
                RecipientControlRequestV1 {
                    protocol_version: RECIPIENT_CONTROL_VERSION,
                    operation: RecipientControlOperationV1::AdmitV2 { uri: secret_uri },
                },
            )
            .await;
            let key = RecordSubscriptionKey {
                workspace_id,
                record_id,
            };
            assert!(matches!(
                response.result,
                RecipientControlResultV1::Admitted {
                    status: RecipientSubscriptionStatusV1 {
                        mount: Some(RecipientMountStatusV1 { running: true, .. }),
                        ..
                    },
                    ..
                }
            ));
            assert!(mounts.stop(key).unwrap());
        }
    }
}
