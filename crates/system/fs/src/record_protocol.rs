//! Authenticated, capability-gated transport for exact file records.
//!
//! Mutual device authentication is completed before a single capability byte
//! is read. Each connection then opens one live [`ShareSession`], registers it
//! for later blob authorization, and performs one operation against the
//! repository key derived from that session's signed workspace/record scope.

use std::io;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use fs_protocol::{
    BearerFileInvite, ContentRevisionV1, FileBindingV1, FileConflictV1, FileInviteV1,
    FileMetadataV1, FileRecordId, FileVersionV1,
};
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tilo_user_identity::RevocationList;
use uuid::Uuid;
use wire_frame::Framing;

use crate::authenticated_transport::{
    AuthenticatedTransportConfig, AuthenticatedTransportError, authenticate_fs_peer_parts_at,
};
use crate::canonical_frame::{self, CanonicalFrameError};
use crate::clock::{unix_now_ms, unix_now_ns};
use crate::identity::PeerCredentials;
use crate::record_access::{RecordAccessBinding, RecordAccessRegistry};
use crate::record_replication::{RecordEnvelopeV1, RecordSnapshot};
use crate::record_repository::{RecordRepository, RecordRepositoryError, RecordRepositoryKey};
use crate::replica_ack::{ReplicaAckRequestV1, ReplicaAckV1};
use crate::share_authority::ShareAuthority;
use crate::share_link::{FileShareLinkError, FileShareLinkV2};
use crate::share_session::{LiveActorCertification, ShareSession};
use tilo_lock::MutexRecover as _;

pub const TILO_FS_RECORD_ALPN: &[u8] = b"tilo-fs-record/6";
pub const RECORD_PROTOCOL_VERSION: u16 = 6;
pub const MAX_RECORD_PROTOCOL_FRAME_BYTES: usize = 256 * 1024;
pub const RECORD_PROTOCOL_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest a host holds a [`RecordProtocolOperationV1::AwaitChange`]
/// before answering [`RecordProtocolResponseV1::Unchanged`]. An
/// idle recipient therefore sends one request a minute, over a connection
/// it already holds.
pub const RECORD_AWAIT_CHANGE_LIMIT: Duration = Duration::from_secs(60);
/// Change awaits one host holds at once. Past it, the host declines an
/// await as it would an unknown operation, and that recipient polls.
pub const MAX_RECORD_CHANGE_AWAITS: usize = 256;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RecordProtocolCredentialV1 {
    Named(FileInviteV1),
    /// Endpoint-bound copied link. Its endpoint id must be this server.
    BearerLinkV2(String),
    /// Secret-bearing invite received over this already-authenticated stream.
    BearerInvite {
        invite: FileInviteV1,
        secret: [u8; 32],
    },
}

impl std::fmt::Debug for RecordProtocolCredentialV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named(invite) => formatter.debug_tuple("Named").field(invite).finish(),
            Self::BearerLinkV2(_) => formatter
                .debug_tuple("BearerLinkV2")
                .field(&"<redacted>")
                .finish(),
            Self::BearerInvite { invite, .. } => formatter
                .debug_struct("BearerInvite")
                .field("invite", invite)
                .field("secret", &"<redacted>")
                .finish(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::large_enum_variant,
    reason = "a wire operation decoded once per request and consumed immediately; boxing the \
              envelope would only add an allocation per write"
)]
pub enum RecordProtocolOperationV1 {
    Read,
    Write(RecordEnvelopeV1),
    /// Submit proof that this authenticated exact-record recipient has made
    /// the named immutable revision durable locally.
    AcknowledgeReplica(ReplicaAckV1),
    /// Ask the exact-record host to prove it durably retains the requested
    /// immutable revision. Read authority is sufficient; this grants no
    /// mutation capability and the session remains the only record scope.
    RequestReplicaAcknowledgement(ReplicaAckRequestV1),
    /// Answer with the record's snapshot once it differs from the one whose
    /// [`RecordProtocolSnapshotV1::digest`] is `known`: at once if it already
    /// does, else when a write or a grant change makes it differ, or
    /// [`RecordProtocolResponseV1::Unchanged`] after
    /// [`RECORD_AWAIT_CHANGE_LIMIT`].
    ///
    /// A host holding all the awaits it may ([`MAX_RECORD_CHANGE_AWAITS`])
    /// answers `InvalidRequest`, and the recipient polls with `Read`. A host
    /// that answers anything else keeps the stream open for the client's
    /// next request.
    AwaitChange {
        known: [u8; 32],
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordProtocolRequestV1 {
    pub protocol_version: u16,
    pub credential: RecordProtocolCredentialV1,
    pub operation: RecordProtocolOperationV1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordProtocolSnapshotV1 {
    pub record_id: FileRecordId,
    pub binding: Option<FileBindingV1>,
    pub metadata: Option<FileMetadataV1>,
    pub content_revision: Option<ContentRevisionV1>,
    pub versions: Vec<FileVersionV1>,
    pub conflicts: Vec<FileConflictV1>,
}

impl RecordProtocolSnapshotV1 {
    /// BLAKE3 of the snapshot's canonical encoding: what a recipient names
    /// in [`RecordProtocolOperationV1::AwaitChange`], and what the host
    /// compares it with. `None` only if the snapshot does not encode, in
    /// which case a host treats it as changed.
    #[must_use]
    pub fn digest(&self) -> Option<[u8; 32]> {
        collab_types::canonical_bytes(self)
            .ok()
            .map(|bytes| *blake3::hash(&bytes).as_bytes())
    }
}

impl From<RecordSnapshot> for RecordProtocolSnapshotV1 {
    fn from(snapshot: RecordSnapshot) -> Self {
        Self {
            record_id: snapshot.record_id,
            binding: snapshot.binding,
            metadata: snapshot.metadata,
            content_revision: snapshot.content_revision,
            versions: snapshot.versions,
            conflicts: snapshot.conflicts,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordProtocolSessionV1 {
    pub session_id: Uuid,
    pub grant_id: Uuid,
    pub workspace_id: Uuid,
    pub record_id: FileRecordId,
}

impl RecordProtocolSessionV1 {
    /// Whether this is a session of what `invite` grants: its workspace and
    /// its record. A recipient accepts a snapshot only from such a session,
    /// never one the host opened on another workspace or record.
    #[must_use]
    pub fn is_for_invite(&self, invite: &fs_protocol::FileInviteV1) -> bool {
        self.workspace_id == invite.workspace_id && self.record_id == invite.record_id
    }
}

impl From<RecordAccessBinding> for RecordProtocolSessionV1 {
    fn from(binding: RecordAccessBinding) -> Self {
        Self {
            session_id: binding.session_id.0,
            grant_id: binding.grant_id,
            workspace_id: binding.workspace_id,
            record_id: binding.record_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::large_enum_variant,
    reason = "Snapshots and replica receipts stay inline so response construction does not require an additional box allocation."
)]
pub enum RecordProtocolResponseV1 {
    Snapshot {
        session: RecordProtocolSessionV1,
        snapshot: RecordProtocolSnapshotV1,
    },
    Applied {
        session: RecordProtocolSessionV1,
    },
    ReplicaAcknowledged {
        session: RecordProtocolSessionV1,
    },
    ReplicaAcknowledgement {
        session: RecordProtocolSessionV1,
        receipt: ReplicaAckV1,
    },
    ReplicaUnavailable,
    Denied,
    NotFound,
    InvalidRequest,
    RequestTooLarge,
    ResponseTooLarge,
    InternalError,
    /// A change await reached its limit with the record still as the
    /// recipient holds it. Appended with
    /// [`RecordProtocolOperationV1::AwaitChange`]; only an await gets it.
    Unchanged {
        session: RecordProtocolSessionV1,
    },
}

/// Optional durable handoff for an accepted record mutation.
///
/// The scope comes from the authenticated, authorized [`ShareSession`], never
/// from request data. Errors are deliberately finite and payload-free so they
/// cannot leak backend details onto the wire.
pub trait RecordMutationSink: Send + Sync {
    /// Durably project one accepted mutation.
    fn record_mutation(
        &self,
        _repository: &RecordRepository,
        scope: RecordAccessBinding,
        envelope: &RecordEnvelopeV1,
        snapshot: &RecordSnapshot,
    ) -> Result<(), RecordMutationSinkError>;

    /// Reconcile repository-wide invariants after a mutation whose visibility
    /// may depend on more than this exact record (currently signed
    /// multi-record transactions). The default keeps simple sinks compatible.
    fn reconcile_repository(
        &self,
        _repository: &RecordRepository,
    ) -> Result<(), RecordMutationSinkError> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RecordMutationSinkError {
    #[error("record mutation sink rejected the mutation")]
    Rejected,
    #[error("record mutation sink is unavailable")]
    Unavailable,
}

#[derive(Debug, Default)]
struct NoopRecordMutationSink;

impl RecordMutationSink for NoopRecordMutationSink {
    fn record_mutation(
        &self,
        _repository: &RecordRepository,
        _scope: RecordAccessBinding,
        _envelope: &RecordEnvelopeV1,
        _snapshot: &RecordSnapshot,
    ) -> Result<(), RecordMutationSinkError> {
        Ok(())
    }
}

/// Router handler for one workspace's durable record repository.
#[derive(Clone)]
pub struct RecordProtocol {
    endpoint: Endpoint,
    credentials: PeerCredentials,
    revocations: Arc<RwLock<RevocationList>>,
    workspace_id: Uuid,
    authority: Arc<ShareAuthority>,
    certification: Arc<dyn LiveActorCertification>,
    access_registry: Arc<RecordAccessRegistry>,
    repository: Arc<RecordRepository>,
    mutation_sink: Arc<dyn RecordMutationSink>,
    availability: Option<Arc<Mutex<crate::availability::AvailabilityRegistry>>>,
    replica_store: Option<Arc<crate::store::ContentStore>>,
    /// Permits for the change awaits held at once
    /// ([`MAX_RECORD_CHANGE_AWAITS`]).
    awaits: Arc<tokio::sync::Semaphore>,
    /// How long one await is held: [`RECORD_AWAIT_CHANGE_LIMIT`].
    await_limit: Duration,
}

impl std::fmt::Debug for RecordProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordProtocol")
            .field("endpoint", &self.endpoint.id())
            .field("workspace_id", &self.workspace_id)
            .finish_non_exhaustive()
    }
}

impl RecordProtocol {
    /// Construct a protocol handler bound to one certified local endpoint.
    pub fn new(
        endpoint: Endpoint,
        credentials: PeerCredentials,
        revocations: Arc<RwLock<RevocationList>>,
        workspace_id: Uuid,
        authority: Arc<ShareAuthority>,
        certification: Arc<dyn LiveActorCertification>,
        access_registry: Arc<RecordAccessRegistry>,
        repository: Arc<RecordRepository>,
    ) -> Result<Self, RecordProtocolError> {
        if credentials.device_public_key() != *endpoint.id().as_bytes() {
            return Err(RecordProtocolError::LocalEndpointMismatch);
        }
        Ok(Self {
            endpoint,
            credentials,
            revocations,
            workspace_id,
            authority,
            certification,
            access_registry,
            repository,
            mutation_sink: Arc::new(NoopRecordMutationSink),
            availability: None,
            replica_store: None,
            awaits: Arc::new(tokio::sync::Semaphore::new(MAX_RECORD_CHANGE_AWAITS)),
            await_limit: RECORD_AWAIT_CHANGE_LIMIT,
        })
    }

    /// Hold change awaits for `limit` instead of
    /// [`RECORD_AWAIT_CHANGE_LIMIT`] (never longer), so a test sees several
    /// in seconds.
    #[must_use]
    pub fn with_await_limit(mut self, limit: Duration) -> Self {
        self.await_limit = limit.min(RECORD_AWAIT_CHANGE_LIMIT);
        self
    }

    /// Install a sink that must accept every successfully applied mutation
    /// before the protocol acknowledges it to the peer.
    #[must_use]
    pub fn with_mutation_sink(mut self, mutation_sink: Arc<dyn RecordMutationSink>) -> Self {
        self.mutation_sink = mutation_sink;
        self
    }

    /// Persist verified exact-recipient durability receipts. Without this
    /// registry the protocol deliberately refuses acknowledgement submissions.
    #[must_use]
    pub fn with_availability(
        mut self,
        availability: Arc<Mutex<crate::availability::AvailabilityRegistry>>,
    ) -> Self {
        self.availability = Some(availability);
        self
    }

    /// Install the durable content backend used to answer reciprocal replica
    /// acknowledgement requests. Without it, the protocol fails closed.
    #[must_use]
    pub fn with_replica_store(mut self, store: Arc<crate::store::ContentStore>) -> Self {
        self.replica_store = Some(store);
        self
    }

    async fn authenticate(
        &self,
        connection: &Connection,
        recv: &mut RecvStream,
        send: &mut SendStream,
    ) -> Result<crate::peer_auth::AuthenticatedFsPeer, RecordProtocolError> {
        let revocations = self
            .revocations
            .read()
            .map_err(|_| RecordProtocolError::RevocationStatePoisoned)?
            .clone();
        Ok(authenticate_fs_peer_parts_at(
            recv,
            send,
            &self.credentials,
            *self.endpoint.id().as_bytes(),
            *connection.remote_id().as_bytes(),
            &revocations,
            unix_now_ns(),
            AuthenticatedTransportConfig::default(),
        )
        .await?)
    }

    /// Serve one stream: one request, or, once the client's first request
    /// is a change await, every request it sends after it.
    /// Returns whether the stream was held for more than one request.
    async fn serve_stream(
        &self,
        connection: &Connection,
        recv: &mut RecvStream,
        send: &mut SendStream,
    ) -> Result<bool, RecordProtocolError> {
        let peer = tokio::time::timeout(
            RECORD_PROTOCOL_TIMEOUT,
            self.authenticate(connection, recv, send),
        )
        .await
        .map_err(|_| RecordProtocolError::TimedOut)??;

        // Capability bytes are read only after the line above succeeds.
        let mut held = false;
        loop {
            let read = if held {
                tokio::time::timeout(RECORD_PROTOCOL_TIMEOUT, read_next_request(recv)).await
            } else {
                tokio::time::timeout(RECORD_PROTOCOL_TIMEOUT, read_request(recv))
                    .await
                    .map(|read| read.map(Some))
            };
            let request = match read {
                Ok(Ok(Some(request))) => request,
                // A held stream ends when its client stops sending: it
                // finished the stream, closed the connection or went quiet.
                Ok(Ok(None) | Err(CanonicalFrameError::Io(_))) | Err(_) if held => break,
                Err(_) => return Err(RecordProtocolError::TimedOut),
                Ok(Err(CanonicalFrameError::TooLarge(_))) => {
                    write_response(send, &RecordProtocolResponseV1::RequestTooLarge).await?;
                    let _ = send.finish();
                    return Ok(held);
                }
                Ok(Ok(None) | Err(_)) => {
                    write_response(send, &RecordProtocolResponseV1::InvalidRequest).await?;
                    let _ = send.finish();
                    return Ok(held);
                }
            };
            let awaits = matches!(
                request.operation,
                RecordProtocolOperationV1::AwaitChange { .. }
            );
            let budget = if awaits {
                self.await_limit.saturating_add(RECORD_PROTOCOL_TIMEOUT)
            } else {
                RECORD_PROTOCOL_TIMEOUT
            };
            let exchange = async {
                let response = self.handle_request(peer.clone(), request).await;
                write_response(send, &response).await?;
                Ok::<_, RecordProtocolError>(response)
            };
            let response = tokio::select! {
                response = tokio::time::timeout(budget, exchange) => {
                    response.map_err(|_| RecordProtocolError::TimedOut)??
                }
                // Nobody is left to answer: do not hold an await for them.
                _ = connection.closed() => return Ok(held),
            };
            // A client that sends an await is new enough to send its next
            // request on this stream; any other client gets one answer.
            held |= awaits && response != RecordProtocolResponseV1::InvalidRequest;
            if !held {
                let _ = send.finish();
                return Ok(false);
            }
        }
        let _ = send.finish();
        Ok(true)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Credential admission and the read, write, and replica-receipt branches share one exact-record session scope and visible denial policy."
    )]
    async fn handle_request(
        &self,
        peer: crate::peer_auth::AuthenticatedFsPeer,
        request: RecordProtocolRequestV1,
    ) -> RecordProtocolResponseV1 {
        if request.protocol_version != RECORD_PROTOCOL_VERSION {
            return RecordProtocolResponseV1::InvalidRequest;
        }
        let session = match request.credential {
            RecordProtocolCredentialV1::Named(invite) => ShareSession::open_named(
                self.workspace_id,
                peer,
                invite,
                Arc::clone(&self.authority),
                Arc::clone(&self.certification),
            ),
            RecordProtocolCredentialV1::BearerInvite { invite, secret } => {
                ShareSession::open_bearer(
                    self.workspace_id,
                    peer,
                    BearerFileInvite {
                        token: invite,
                        secret,
                    },
                    Arc::clone(&self.authority),
                    Arc::clone(&self.certification),
                )
            }
            RecordProtocolCredentialV1::BearerLinkV2(uri) => {
                let link = match FileShareLinkV2::decode_uri(&uri) {
                    Ok(link) if link.endpoint.id == self.endpoint.id() => link,
                    _ => return RecordProtocolResponseV1::Denied,
                };
                ShareSession::open_bearer(
                    self.workspace_id,
                    peer,
                    BearerFileInvite {
                        token: link.invite.clone(),
                        secret: *link.bearer_secret(),
                    },
                    Arc::clone(&self.authority),
                    Arc::clone(&self.certification),
                )
            }
        };
        let session = match session {
            Ok(session) => Arc::new(session),
            Err(_) => return RecordProtocolResponseV1::Denied,
        };
        let binding = self.access_registry.register(Arc::clone(&session));
        let key = RecordRepositoryKey {
            workspace_id: session.workspace_id(),
            record_id: session.record_id(),
        };
        // No client-provided repository key exists: the exact signed session
        // scope is the only key this connection can address.
        match request.operation {
            RecordProtocolOperationV1::Read => {
                match self
                    .repository
                    .snapshot(key, &session.actor(), session.as_ref())
                {
                    Ok(snapshot) => RecordProtocolResponseV1::Snapshot {
                        session: binding.into(),
                        snapshot: snapshot.into(),
                    },
                    Err(RecordRepositoryError::RecordNotFound) => {
                        RecordProtocolResponseV1::NotFound
                    }
                    Err(RecordRepositoryError::Replication(_)) => RecordProtocolResponseV1::Denied,
                    Err(_) => RecordProtocolResponseV1::InternalError,
                }
            }
            RecordProtocolOperationV1::Write(envelope) => {
                match self.repository.apply(key, &envelope, session.as_ref()) {
                    Ok(()) => {}
                    Err(RecordRepositoryError::Replication(
                        crate::record_replication::RecordReplicationError::Replay { .. },
                    )) if self.repository.is_latest_envelope(key, &envelope) => {}
                    Err(
                        RecordRepositoryError::Replication(_)
                        | RecordRepositoryError::CrossRecordEnvelope,
                    ) => {
                        return RecordProtocolResponseV1::Denied;
                    }
                    Err(_) => return RecordProtocolResponseV1::InternalError,
                }
                let Ok(snapshot) =
                    self.repository
                        .snapshot(key, &session.actor(), session.as_ref())
                else {
                    return RecordProtocolResponseV1::InternalError;
                };
                let cross_record = matches!(
                    envelope.operation,
                    crate::record_replication::RecordOperationV1::PrepareTransaction(_)
                        | crate::record_replication::RecordOperationV1::CommitTransaction { .. }
                );
                match self.mutation_sink.record_mutation(
                    &self.repository,
                    binding,
                    &envelope,
                    &snapshot,
                ) {
                    Ok(()) => {
                        if cross_record
                            && self
                                .mutation_sink
                                .reconcile_repository(&self.repository)
                                .is_err()
                        {
                            return RecordProtocolResponseV1::InternalError;
                        }
                        RecordProtocolResponseV1::Applied {
                            session: binding.into(),
                        }
                    }
                    Err(_) => RecordProtocolResponseV1::InternalError,
                }
            }
            RecordProtocolOperationV1::AcknowledgeReplica(receipt) => {
                let Some(availability) = &self.availability else {
                    return RecordProtocolResponseV1::InternalError;
                };
                if receipt.workspace_id != key.workspace_id
                    || receipt.revision.record_id != key.record_id
                {
                    return RecordProtocolResponseV1::Denied;
                }
                let snapshot =
                    match self
                        .repository
                        .snapshot(key, &session.actor(), session.as_ref())
                    {
                        Ok(snapshot) => snapshot,
                        Err(RecordRepositoryError::RecordNotFound) => {
                            return RecordProtocolResponseV1::NotFound;
                        }
                        Err(RecordRepositoryError::Replication(_)) => {
                            return RecordProtocolResponseV1::Denied;
                        }
                        Err(_) => return RecordProtocolResponseV1::InternalError,
                    };
                let revision_matches = snapshot.content_revision == Some(receipt.revision)
                    && snapshot.versions.iter().any(|version| {
                        version.id == receipt.revision.version_id
                            && version.record_id == receipt.revision.record_id
                            && version.content_hash == receipt.revision.content_hash
                            && version.metadata_hash == receipt.revision.metadata_hash
                    });
                if !revision_matches {
                    return RecordProtocolResponseV1::Denied;
                }
                let observed_at = unix_now_ms();
                let Ok(observation) = receipt.verify_claim(
                    session.authenticated_peer(),
                    collab_types::Ed25519PublicKey(self.credentials.device_public_key()),
                    observed_at,
                ) else {
                    return RecordProtocolResponseV1::Denied;
                };
                let mut availability = availability.lock_recover();
                let parent = availability
                    .get(receipt.revision.record_id)
                    .and_then(|record| record.parent);
                let tracked = availability.track_current_revision(
                    receipt.revision,
                    crate::availability::RecordKind::File,
                    parent,
                    observed_at,
                );
                let result = tracked.and_then(|_| {
                    availability.observe_replica(
                        key.record_id,
                        collab_types::Ed25519PublicKey(self.credentials.device_public_key()),
                        observation,
                        observed_at,
                    )
                });
                drop(availability);
                match result {
                    Ok(()) => RecordProtocolResponseV1::ReplicaAcknowledged {
                        session: binding.into(),
                    },
                    Err(
                        crate::availability::AvailabilityError::Io(_)
                        | crate::availability::AvailabilityError::Decode(_),
                    ) => RecordProtocolResponseV1::InternalError,
                    Err(_) => RecordProtocolResponseV1::Denied,
                }
            }
            RecordProtocolOperationV1::AwaitChange { known } => {
                self.await_change(key, &session, binding, known).await
            }
            RecordProtocolOperationV1::RequestReplicaAcknowledgement(request) => {
                if request.validate().is_err()
                    || request.workspace_id != key.workspace_id
                    || request.revision.record_id != key.record_id
                {
                    return RecordProtocolResponseV1::Denied;
                }
                let snapshot =
                    match self
                        .repository
                        .snapshot(key, &session.actor(), session.as_ref())
                    {
                        Ok(snapshot) => snapshot,
                        Err(RecordRepositoryError::RecordNotFound) => {
                            return RecordProtocolResponseV1::NotFound;
                        }
                        Err(RecordRepositoryError::Replication(_)) => {
                            return RecordProtocolResponseV1::Denied;
                        }
                        Err(_) => return RecordProtocolResponseV1::InternalError,
                    };
                let revision_matches = snapshot.content_revision == Some(request.revision)
                    && snapshot.versions.iter().any(|version| {
                        version.id == request.revision.version_id
                            && version.record_id == request.revision.record_id
                            && version.content_hash == request.revision.content_hash
                            && version.metadata_hash == request.revision.metadata_hash
                    });
                if !revision_matches {
                    return RecordProtocolResponseV1::Denied;
                }
                let Some(store) = &self.replica_store else {
                    return RecordProtocolResponseV1::ReplicaUnavailable;
                };
                if store
                    .verify_durable_revision(&request.revision)
                    .await
                    .is_err()
                {
                    return RecordProtocolResponseV1::ReplicaUnavailable;
                }
                let actor = fs_protocol::FileActor {
                    person: collab_types::Ed25519PublicKey(self.credentials.user_public_key()),
                    device: collab_types::Ed25519PublicKey(self.credentials.device_public_key()),
                };
                match ReplicaAckV1::signed(
                    &request,
                    actor,
                    unix_now_ms(),
                    self.credentials.signing_key(),
                ) {
                    Ok(receipt) => RecordProtocolResponseV1::ReplicaAcknowledgement {
                        session: binding.into(),
                        receipt,
                    },
                    Err(_) => RecordProtocolResponseV1::InternalError,
                }
            }
        }
    }

    /// Answer a change await: at once if the record already differs from
    /// the snapshot the recipient holds, otherwise when a write or a grant
    /// change makes it differ, or `Unchanged` once the await limit passes.
    ///
    /// Every wake reads the snapshot through the session again, which
    /// rechecks certification, grant, revocation and expiry, and so does
    /// the final `Unchanged`: a session revoked or expired while it waited
    /// is answered `Denied`, never with data. A grant revocation wakes the
    /// await at once; a device revocation is seen at the latest when the
    /// limit passes, and no snapshot is sent before it is checked.
    async fn await_change(
        &self,
        key: RecordRepositoryKey,
        session: &ShareSession,
        binding: RecordAccessBinding,
        known: [u8; 32],
    ) -> RecordProtocolResponseV1 {
        let Ok(_permit) = Arc::clone(&self.awaits).try_acquire_owned() else {
            return RecordProtocolResponseV1::InvalidRequest;
        };
        let mut writes = self.repository.subscribe_writes();
        let mut grants = self.authority.subscribe_changes();
        let limit = tokio::time::sleep(self.await_limit);
        tokio::pin!(limit);
        loop {
            // Marked seen before the read below, so a change made after it
            // still wakes the wait.
            writes.borrow_and_update();
            grants.borrow_and_update();
            let snapshot = match self.repository.snapshot(key, &session.actor(), session) {
                Ok(snapshot) => RecordProtocolSnapshotV1::from(snapshot),
                Err(RecordRepositoryError::RecordNotFound) => {
                    return RecordProtocolResponseV1::NotFound;
                }
                Err(RecordRepositoryError::Replication(_)) => {
                    return RecordProtocolResponseV1::Denied;
                }
                Err(_) => return RecordProtocolResponseV1::InternalError,
            };
            if snapshot.digest() != Some(known) {
                return RecordProtocolResponseV1::Snapshot {
                    session: binding.into(),
                    snapshot,
                };
            }
            if limit.is_elapsed() {
                return RecordProtocolResponseV1::Unchanged {
                    session: binding.into(),
                };
            }
            tokio::select! {
                Ok(()) = writes.changed() => {}
                Ok(()) = grants.changed() => {}
                () = &mut limit => {}
            }
        }
    }
}

impl ProtocolHandler for RecordProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        match self.serve_stream(&connection, &mut recv, &mut send).await {
            Ok(false) => {
                // Keep the QUIC connection alive until the client has observed
                // the finished response stream. Returning immediately lets the
                // router close the connection before buffered bytes arrive.
                connection.closed().await;
                Ok(())
            }
            Ok(true) => {
                // A held stream ends only once its client stopped sending,
                // which it does after reading every answer it asked for.
                connection.close(0u32.into(), b"");
                Ok(())
            }
            Err(RecordProtocolError::TimedOut) => Err(AcceptError::from_err(io::Error::new(
                io::ErrorKind::TimedOut,
                "record protocol timed out",
            ))),
            Err(error) => Err(AcceptError::from_err(io::Error::other(error.to_string()))),
        }
    }
}

/// Bounded client for one authenticated record operation per connection.
#[derive(Clone)]
pub struct RecordProtocolClient {
    endpoint: Endpoint,
    credentials: PeerCredentials,
    revocations: Arc<RwLock<RevocationList>>,
}

impl std::fmt::Debug for RecordProtocolClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordProtocolClient")
            .field("endpoint", &self.endpoint.id())
            .finish_non_exhaustive()
    }
}

impl RecordProtocolClient {
    /// Construct a client bound to one certified local endpoint.
    pub fn new(
        endpoint: Endpoint,
        credentials: PeerCredentials,
        revocations: Arc<RwLock<RevocationList>>,
    ) -> Result<Self, RecordProtocolClientError> {
        if credentials.device_public_key() != *endpoint.id().as_bytes() {
            return Err(RecordProtocolClientError::LocalEndpointMismatch);
        }
        Ok(Self {
            endpoint,
            credentials,
            revocations,
        })
    }

    /// Dial a named invite's separately supplied endpoint.
    pub async fn request_named(
        &self,
        server: iroh_base::EndpointAddr,
        invite: FileInviteV1,
        operation: RecordProtocolOperationV1,
    ) -> Result<RecordProtocolResponseV1, RecordProtocolClientError> {
        self.request(server, RecordProtocolCredentialV1::Named(invite), operation)
            .await
    }

    /// Validate an endpoint-bound copied link before dialing its signed
    /// endpoint. The secret-bearing URI is constructed only after validation.
    pub async fn request_bearer_link(
        &self,
        link: &FileShareLinkV2,
        operation: RecordProtocolOperationV1,
    ) -> Result<RecordProtocolResponseV1, RecordProtocolClientError> {
        let now = unix_now_ms();
        link.validate_at(now)?;
        let server = link.endpoint.clone();
        let uri = link.encode_uri_at(now)?;
        self.request(
            server,
            RecordProtocolCredentialV1::BearerLinkV2(uri),
            operation,
        )
        .await
    }

    /// Send a direct bearer invite over the already authenticated transport.
    pub async fn request_bearer_invite(
        &self,
        server: iroh_base::EndpointAddr,
        invite: BearerFileInvite,
        operation: RecordProtocolOperationV1,
    ) -> Result<RecordProtocolResponseV1, RecordProtocolClientError> {
        self.request(
            server,
            RecordProtocolCredentialV1::BearerInvite {
                invite: invite.token,
                secret: invite.secret,
            },
            operation,
        )
        .await
    }

    /// Request and verify the exact-share host's durable receipt for one
    /// revision. Verification is bound to the mutually authenticated server,
    /// caller nonce, exact revision, and this client's device identity before
    /// any observation is returned to availability policy.
    pub async fn request_bearer_link_replica_ack(
        &self,
        link: &FileShareLinkV2,
        request: ReplicaAckRequestV1,
    ) -> Result<crate::availability::ReplicaObservation, RecordProtocolClientError> {
        let now = unix_now_ms();
        link.validate_at(now)?;
        let server = link.endpoint.clone();
        let uri = link.encode_uri_at(now)?;
        let (response, peer) = self
            .request_authenticated(
                server,
                RecordProtocolCredentialV1::BearerLinkV2(uri),
                RecordProtocolOperationV1::RequestReplicaAcknowledgement(request),
            )
            .await?;
        verified_replica_ack(response, &request, &peer, self.local_device())
    }

    /// Dial `server` and authenticate both ends once, for a stream of
    /// requests. See [`RecordProtocolStream`].
    pub async fn open_stream(
        &self,
        server: iroh_base::EndpointAddr,
    ) -> Result<RecordProtocolStream, RecordProtocolClientError> {
        tokio::time::timeout(RECORD_PROTOCOL_TIMEOUT, self.connect(server))
            .await
            .map_err(|_| RecordProtocolClientError::Timeout)?
    }

    fn local_device(&self) -> collab_types::Ed25519PublicKey {
        collab_types::Ed25519PublicKey(self.credentials.device_public_key())
    }

    async fn connect(
        &self,
        server: iroh_base::EndpointAddr,
    ) -> Result<RecordProtocolStream, RecordProtocolClientError> {
        let revocations = self
            .revocations
            .read()
            .map_err(|_| RecordProtocolClientError::RevocationStatePoisoned)?
            .clone();
        let connection = self
            .endpoint
            .connect(server.clone(), TILO_FS_RECORD_ALPN)
            .await
            .map_err(|error| RecordProtocolClientError::Transport(error.to_string()))?;
        if connection.remote_id() != server.id {
            return Err(RecordProtocolClientError::ServerEndpointMismatch);
        }
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| RecordProtocolClientError::Transport(error.to_string()))?;
        let peer = authenticate_fs_peer_parts_at(
            &mut recv,
            &mut send,
            &self.credentials,
            *self.endpoint.id().as_bytes(),
            *server.id.as_bytes(),
            &revocations,
            unix_now_ns(),
            AuthenticatedTransportConfig::default(),
        )
        .await?;
        Ok(RecordProtocolStream {
            connection,
            send,
            recv,
            peer,
            server: server.id,
            local_device: self.local_device(),
            held: false,
            ended: false,
        })
    }

    async fn request(
        &self,
        server: iroh_base::EndpointAddr,
        credential: RecordProtocolCredentialV1,
        operation: RecordProtocolOperationV1,
    ) -> Result<RecordProtocolResponseV1, RecordProtocolClientError> {
        self.request_authenticated(server, credential, operation)
            .await
            .map(|(response, _)| response)
    }

    async fn request_authenticated(
        &self,
        server: iroh_base::EndpointAddr,
        credential: RecordProtocolCredentialV1,
        operation: RecordProtocolOperationV1,
    ) -> Result<
        (
            RecordProtocolResponseV1,
            crate::peer_auth::AuthenticatedFsPeer,
        ),
        RecordProtocolClientError,
    > {
        let budget = request_budget(&operation);
        let exchange = async {
            let mut stream = self.connect(server).await?;
            let response = stream.send_request(credential, operation).await?;
            let RecordProtocolStream {
                connection, peer, ..
            } = stream;
            connection.close(0u32.into(), b"");
            Ok((response, peer))
        };
        tokio::time::timeout(budget, exchange)
            .await
            .map_err(|_| RecordProtocolClientError::Timeout)?
    }
}

/// How long a client waits for the answer to `operation`: the protocol
/// timeout, plus the await limit for a change await.
fn request_budget(operation: &RecordProtocolOperationV1) -> Duration {
    match operation {
        RecordProtocolOperationV1::AwaitChange { .. } => {
            RECORD_AWAIT_CHANGE_LIMIT.saturating_add(RECORD_PROTOCOL_TIMEOUT)
        }
        _ => RECORD_PROTOCOL_TIMEOUT,
    }
}

/// The host's replica receipt in `response`, verified against the request,
/// the authenticated host and this device.
fn verified_replica_ack(
    response: RecordProtocolResponseV1,
    request: &ReplicaAckRequestV1,
    peer: &crate::peer_auth::AuthenticatedFsPeer,
    local_device: collab_types::Ed25519PublicKey,
) -> Result<crate::availability::ReplicaObservation, RecordProtocolClientError> {
    let receipt = match response {
        RecordProtocolResponseV1::ReplicaAcknowledgement { receipt, .. } => receipt,
        RecordProtocolResponseV1::Denied | RecordProtocolResponseV1::NotFound => {
            return Err(RecordProtocolClientError::ReplicaDenied);
        }
        RecordProtocolResponseV1::ReplicaUnavailable => {
            return Err(RecordProtocolClientError::ReplicaUnavailable);
        }
        _ => return Err(RecordProtocolClientError::InvalidResponse),
    };
    receipt
        .verify_for(request, peer, local_device, unix_now_ms())
        .map_err(RecordProtocolClientError::ReplicaVerification)
}

/// One authenticated connection to a record host that carries a share
/// recipient's requests one after another.
///
/// Its first request is a change await. A host that knows the operation
/// keeps the stream after answering, and the recipient sends its next
/// awaits, replica requests and receipts on it: no further connection and
/// no further peer handshake while the record is idle. Every request still
/// carries its credential, and the host authorizes each one from scratch.
/// A host that declines the await answers `InvalidRequest` and ends the
/// stream; [`Self::is_held`] then stays false.
pub struct RecordProtocolStream {
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
    peer: crate::peer_auth::AuthenticatedFsPeer,
    server: iroh_base::EndpointId,
    local_device: collab_types::Ed25519PublicKey,
    /// The host answered an await, so it reads the next request.
    held: bool,
    /// The host has answered its last request on this stream, or the
    /// stream failed: nothing more can be sent on it.
    ended: bool,
}

impl std::fmt::Debug for RecordProtocolStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RecordProtocolStream")
            .field("server", &self.server)
            .field("held", &self.held)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl RecordProtocolStream {
    /// Whether the host keeps this stream for further requests: it has
    /// answered a change await, and nothing has failed since.
    #[must_use]
    pub fn is_held(&self) -> bool {
        self.held && !self.ended
    }

    /// Whether another request can still be sent on this stream.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.ended
    }

    /// Send `operation` for a named invite.
    pub async fn request_named(
        &mut self,
        invite: FileInviteV1,
        operation: RecordProtocolOperationV1,
    ) -> Result<RecordProtocolResponseV1, RecordProtocolClientError> {
        self.send_request(RecordProtocolCredentialV1::Named(invite), operation)
            .await
    }

    /// Send `operation` for a copied link, validated first. The link must
    /// name the endpoint this stream is connected to.
    pub async fn request_bearer_link(
        &mut self,
        link: &FileShareLinkV2,
        operation: RecordProtocolOperationV1,
    ) -> Result<RecordProtocolResponseV1, RecordProtocolClientError> {
        let uri = self.link_uri(link)?;
        self.send_request(RecordProtocolCredentialV1::BearerLinkV2(uri), operation)
            .await
    }

    /// [`RecordProtocolClient::request_bearer_link_replica_ack`] on this
    /// stream.
    ///
    /// # Errors
    ///
    /// As [`Self::request_bearer_link`], and when the host denies the
    /// request or its receipt does not verify.
    pub async fn request_bearer_link_replica_ack(
        &mut self,
        link: &FileShareLinkV2,
        request: ReplicaAckRequestV1,
    ) -> Result<crate::availability::ReplicaObservation, RecordProtocolClientError> {
        let uri = self.link_uri(link)?;
        let response = self
            .send_request(
                RecordProtocolCredentialV1::BearerLinkV2(uri),
                RecordProtocolOperationV1::RequestReplicaAcknowledgement(request),
            )
            .await?;
        verified_replica_ack(response, &request, &self.peer, self.local_device)
    }

    /// Close the connection.
    pub fn close(self) {
        self.connection.close(0u32.into(), b"");
    }

    fn link_uri(&self, link: &FileShareLinkV2) -> Result<String, RecordProtocolClientError> {
        let now = unix_now_ms();
        link.validate_at(now)?;
        if link.endpoint.id != self.server {
            return Err(RecordProtocolClientError::ServerEndpointMismatch);
        }
        Ok(link.encode_uri_at(now)?)
    }

    async fn send_request(
        &mut self,
        credential: RecordProtocolCredentialV1,
        operation: RecordProtocolOperationV1,
    ) -> Result<RecordProtocolResponseV1, RecordProtocolClientError> {
        if self.ended {
            return Err(RecordProtocolClientError::Transport(
                "record stream has ended".into(),
            ));
        }
        // Ended unless this exchange completes and the host keeps the stream.
        self.ended = true;
        let awaits = matches!(operation, RecordProtocolOperationV1::AwaitChange { .. });
        let budget = request_budget(&operation);
        let request = RecordProtocolRequestV1 {
            protocol_version: RECORD_PROTOCOL_VERSION,
            credential,
            operation,
        };
        let exchange = async {
            write_request(&mut self.send, &request).await?;
            read_response(&mut self.recv).await
        };
        let response = tokio::time::timeout(budget, exchange)
            .await
            .map_err(|_| RecordProtocolClientError::Timeout)??;
        self.held |= awaits && response != RecordProtocolResponseV1::InvalidRequest;
        self.ended = !self.held;
        Ok(response)
    }
}

#[derive(Debug, Error)]
pub enum RecordProtocolClientError {
    #[error("record client credentials do not match its local endpoint")]
    LocalEndpointMismatch,
    #[error("record client revocation state is poisoned")]
    RevocationStatePoisoned,
    #[error("connected server does not match the requested endpoint")]
    ServerEndpointMismatch,
    #[error("record client transport failed: {0}")]
    Transport(String),
    #[error("record peer authentication failed: {0}")]
    Authentication(#[from] AuthenticatedTransportError),
    #[error("record share link is invalid: {0}")]
    Link(#[from] FileShareLinkError),
    #[error("record request exceeds its wire bound")]
    RequestTooLarge,
    #[error("record request could not be encoded canonically")]
    InvalidRequest,
    #[error("record response exceeds its wire bound")]
    ResponseTooLarge,
    #[error("record response is malformed or non-canonical")]
    InvalidResponse,
    #[error("record protocol exchange timed out")]
    Timeout,
    #[error("exact-share host denied the replica acknowledgement request")]
    ReplicaDenied,
    #[error("exact-share host does not durably retain the requested revision")]
    ReplicaUnavailable,
    #[error("exact-share host returned an invalid replica receipt: {0}")]
    ReplicaVerification(#[source] crate::replica_ack::ReplicaAckError),
}

#[derive(Debug, Error)]
pub enum RecordProtocolError {
    #[error("record protocol credentials do not match the local endpoint")]
    LocalEndpointMismatch,
    #[error("record protocol revocation state is poisoned")]
    RevocationStatePoisoned,
    #[error("record peer authentication failed: {0}")]
    Authentication(#[from] AuthenticatedTransportError),
    #[error("record protocol transport failed: {0}")]
    Io(#[from] io::Error),
    #[error("record protocol canonical encoding failed: {0}")]
    Canonical(#[from] collab_types::canonical::CanonicalError),
    #[error("record protocol timed out")]
    TimedOut,
}

/// Each record request and response: a big-endian `u32` length, then the
/// canonical encoding.
const RECORD_PROTOCOL_FRAMING: Framing = Framing::big_endian(MAX_RECORD_PROTOCOL_FRAME_BYTES);

async fn write_request<W>(
    send: &mut W,
    request: &RecordProtocolRequestV1,
) -> Result<(), RecordProtocolClientError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    canonical_frame::write(RECORD_PROTOCOL_FRAMING, send, request)
        .await
        .map_err(|error| match error {
            CanonicalFrameError::Io(error) => {
                RecordProtocolClientError::Transport(error.to_string())
            }
            CanonicalFrameError::Empty | CanonicalFrameError::TooLarge(_) => {
                RecordProtocolClientError::RequestTooLarge
            }
            CanonicalFrameError::Encode(_) | CanonicalFrameError::Decode(_) => {
                RecordProtocolClientError::InvalidRequest
            }
        })?;
    send.flush()
        .await
        .map_err(|error| RecordProtocolClientError::Transport(error.to_string()))?;
    Ok(())
}

async fn read_response<R>(
    recv: &mut R,
) -> Result<RecordProtocolResponseV1, RecordProtocolClientError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    canonical_frame::read_required(RECORD_PROTOCOL_FRAMING, recv)
        .await
        .map_err(|error| match error {
            CanonicalFrameError::Io(error) => {
                RecordProtocolClientError::Transport(error.to_string())
            }
            CanonicalFrameError::TooLarge(_) => RecordProtocolClientError::ResponseTooLarge,
            CanonicalFrameError::Empty
            | CanonicalFrameError::Encode(_)
            | CanonicalFrameError::Decode(_) => RecordProtocolClientError::InvalidResponse,
        })
}

async fn read_request<R>(recv: &mut R) -> Result<RecordProtocolRequestV1, CanonicalFrameError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    canonical_frame::read_required(RECORD_PROTOCOL_FRAMING, recv).await
}

/// A held stream's next request, or `None` if the client ended the stream
/// before starting one.
async fn read_next_request<R>(
    recv: &mut R,
) -> Result<Option<RecordProtocolRequestV1>, CanonicalFrameError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    canonical_frame::read(RECORD_PROTOCOL_FRAMING, recv).await
}

async fn write_response<W>(
    send: &mut W,
    response: &RecordProtocolResponseV1,
) -> Result<(), RecordProtocolError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let mut bytes = collab_types::canonical_bytes(response)?;
    if bytes.len() > RECORD_PROTOCOL_FRAMING.max_len() {
        bytes = collab_types::canonical_bytes(&RecordProtocolResponseV1::ResponseTooLarge)?;
    }
    RECORD_PROTOCOL_FRAMING
        .write_async(send, &bytes)
        .await
        .map_err(io::Error::other)?;
    send.flush().await.map_err(io::Error::other)?;
    Ok(())
}

#[cfg(test)]
mod await_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use collab_types::Ed25519PublicKey;
    use ed25519_dalek::SigningKey;
    use fs_protocol::{
        FileActor, FileInviteSpec, FileInviteSubject, FilePermission, IndexStatus,
        mint_bearer_file_invite, mint_file_invite,
    };
    use iroh::address_lookup::memory::MemoryLookup;
    use iroh::endpoint::presets;
    use iroh::protocol::Router;
    use rand::{SeedableRng, rngs::StdRng};
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, IdentityPaths, adopt_device_key,
        persist::write_device_cert, sign_device_cert,
    };

    static NEXT_PARTY: AtomicU64 = AtomicU64::new(100);

    /// A recipient takes a snapshot only from a session on the invite's
    /// workspace and record.
    #[test]
    fn a_session_on_another_workspace_or_record_is_not_the_invites() {
        let workspace = Uuid::new_v4();
        let record = FileRecordId::new();
        let issuer = SigningKey::from_bytes(&[7; 32]);
        let mut rng = StdRng::seed_from_u64(9);
        let invite = mint_bearer_file_invite(
            FileInviteSpec {
                workspace_id: workspace,
                record_id: record,
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
        .unwrap()
        .token;
        let session = RecordProtocolSessionV1 {
            session_id: Uuid::new_v4(),
            grant_id: invite.id,
            workspace_id: workspace,
            record_id: record,
        };
        assert!(session.is_for_invite(&invite));
        let other_workspace = RecordProtocolSessionV1 {
            workspace_id: Uuid::new_v4(),
            ..session
        };
        assert!(!other_workspace.is_for_invite(&invite));
        let other_record = RecordProtocolSessionV1 {
            record_id: FileRecordId::new(),
            ..session
        };
        assert!(!other_record.is_for_invite(&invite));
    }

    pub(super) struct Party {
        pub(super) endpoint: Endpoint,
        pub(super) credentials: PeerCredentials,
        pub(super) device: SigningKey,
    }

    pub(super) async fn party() -> Party {
        let n = NEXT_PARTY.fetch_add(1, Ordering::Relaxed);
        let device_seed = *blake3::hash(&n.to_le_bytes()).as_bytes();
        let root_seed = *blake3::hash(&n.wrapping_add(99_999).to_le_bytes()).as_bytes();
        let device = SigningKey::from_bytes(&device_seed);
        let root = SigningKey::from_bytes(&root_seed);
        let now = unix_now_ns();
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: format!("record-test-{n}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now.saturating_sub(1),
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(temp.path());
        adopt_device_key(&paths, &device_seed).unwrap();
        write_device_cert(&paths.device_cert, &cert).unwrap();
        let certified = crate::identity::load_certified_mount_identity(&paths, now).unwrap();
        let (mount, credentials) = certified.into_parts();
        let lookup = MemoryLookup::new();
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(mount.secret)
            .address_lookup(lookup)
            .bind()
            .await
            .unwrap();
        Party {
            endpoint,
            credentials,
            device,
        }
    }

    pub(super) struct AlwaysCurrent;
    impl LiveActorCertification for AlwaysCurrent {
        fn certification_is_current(
            &self,
            _peer: &crate::peer_auth::AuthenticatedFsPeer,
            _now_ns: u64,
        ) -> bool {
            true
        }
    }

    #[derive(Default)]
    struct RecordingMutationSink {
        calls: Mutex<Vec<(RecordAccessBinding, RecordEnvelopeV1)>>,
        fail: AtomicBool,
    }

    impl RecordMutationSink for RecordingMutationSink {
        fn record_mutation(
            &self,
            _repository: &RecordRepository,
            scope: RecordAccessBinding,
            envelope: &RecordEnvelopeV1,
            _snapshot: &RecordSnapshot,
        ) -> Result<(), RecordMutationSinkError> {
            self.calls.lock().unwrap().push((scope, envelope.clone()));
            if self.fail.load(Ordering::SeqCst) {
                Err(RecordMutationSinkError::Unavailable)
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(
        clippy::too_many_lines,
        reason = "The authenticated lifecycle checks named edits, read-only bearer access, projection retry, cross-record denial, and durable host replica receipts."
    )] // Full authenticated protocol lifecycle scenario.
    async fn authenticated_read_only_share_gets_persisted_exact_host_receipt_without_write_or_sibling_scope()
     {
        let server = party().await;
        let client = party().await;
        let workspace = Uuid::new_v4();
        let record = FileRecordId::new();
        let other_record = FileRecordId::new();
        let issuer = SigningKey::from_bytes(&[88; 32]);
        let issuer_public = Ed25519PublicKey::from_bytes(issuer.verifying_key().to_bytes());
        let token = mint_file_invite(
            FileInviteSpec {
                workspace_id: workspace,
                record_id: record,
                subject: FileInviteSubject::Person(Ed25519PublicKey::from_bytes(
                    client.credentials.user_public_key(),
                )),
                permission: FilePermission::Edit,
                expires_at_ms: None,
                max_redemptions: None,
            },
            &issuer,
        )
        .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let authority = Arc::new(ShareAuthority::open(temp.path().join("shares.json")).unwrap());
        authority.register(&token, &[issuer_public], 1).unwrap();
        let repository = Arc::new(RecordRepository::open(temp.path().join("records")).unwrap());
        let registry = Arc::new(RecordAccessRegistry::new(16, 60_000).unwrap());
        let availability = Arc::new(Mutex::new(
            crate::availability::AvailabilityRegistry::open(temp.path().join("availability.json"))
                .unwrap(),
        ));
        let replica_store = Arc::new(crate::store::ContentStore::memory());
        let replica_payload = b"host-durable exact-share bytes";
        let replica_manifest = replica_store
            .write_content(replica_payload)
            .await
            .unwrap()
            .unwrap();
        let mutation_sink = Arc::new(RecordingMutationSink::default());
        let handler = RecordProtocol::new(
            server.endpoint.clone(),
            server.credentials.clone(),
            Arc::new(RwLock::new(RevocationList::new())),
            workspace,
            Arc::clone(&authority),
            Arc::new(AlwaysCurrent),
            Arc::clone(&registry),
            Arc::clone(&repository),
        )
        .unwrap()
        .with_mutation_sink(mutation_sink.clone())
        .with_availability(Arc::clone(&availability))
        .with_replica_store(Arc::clone(&replica_store));
        let router = Router::builder(server.endpoint.clone())
            .accept(TILO_FS_RECORD_ALPN, handler)
            .spawn();

        // Sending a perfectly valid capability request before mutual auth
        // must never open a session or touch the repository.
        {
            let raw_connection = client
                .endpoint
                .connect(server.endpoint.addr(), TILO_FS_RECORD_ALPN)
                .await
                .unwrap();
            let (mut raw_send, mut raw_recv) = raw_connection.open_bi().await.unwrap();
            let premature = RecordProtocolRequestV1 {
                protocol_version: RECORD_PROTOCOL_VERSION,
                credential: RecordProtocolCredentialV1::Named(token.clone()),
                operation: RecordProtocolOperationV1::Read,
            };
            let bytes = collab_types::canonical_bytes(&premature).unwrap();
            raw_send
                .write_all(&u32::try_from(bytes.len()).unwrap().to_be_bytes())
                .await
                .unwrap();
            raw_send.write_all(&bytes).await.unwrap();
            raw_send.finish().unwrap();
            // Its answer, or the connection closing, ends the stream once
            // the server has read the request and dealt with it.
            let _ = tilo_test_env::within(
                "the server to answer the unauthenticated request",
                raw_recv.read_to_end(64 * 1024),
            )
            .await;
        }
        assert!(registry.is_empty());
        assert!(repository.is_empty());

        let client_api = RecordProtocolClient::new(
            client.endpoint.clone(),
            client.credentials.clone(),
            Arc::new(RwLock::new(RevocationList::new())),
        )
        .unwrap();

        let actor = FileActor {
            person: Ed25519PublicKey::from_bytes(client.credentials.user_public_key()),
            device: Ed25519PublicKey::from_bytes(client.credentials.device_public_key()),
        };
        let metadata = FileMetadataV1 {
            schema_version: fs_protocol::files::FILE_METADATA_VERSION,
            record_id: record,
            content_hash: None,
            byte_len: 12,
            mime: Some("text/plain".into()),
            previews: vec![],
            latest_actor: Some(actor),
            observed_at_ms: 1,
            index_status: IndexStatus::Pending,
            properties: None,
        };
        let envelope = RecordEnvelopeV1::signed(
            record,
            actor,
            1,
            crate::record_replication::RecordOperationV1::SetMetadata(metadata.clone()),
            &client.device,
        )
        .unwrap();
        let accepted_envelope = envelope.clone();
        assert!(matches!(
            client_api
                .request_named(
                    server.endpoint.addr(),
                    token.clone(),
                    RecordProtocolOperationV1::Write(envelope)
                )
                .await
                .unwrap(),
            RecordProtocolResponseV1::Applied { .. }
        ));
        {
            let calls = mutation_sink.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0.workspace_id, workspace);
            assert_eq!(calls[0].0.record_id, record);
            assert_eq!(calls[0].0.person, actor.person);
            assert_eq!(calls[0].0.device, actor.device);
            assert_eq!(calls[0].1, accepted_envelope);
            drop(calls);
        }
        match client_api
            .request_named(
                server.endpoint.addr(),
                token.clone(),
                RecordProtocolOperationV1::Read,
            )
            .await
            .unwrap()
        {
            RecordProtocolResponseV1::Snapshot { snapshot, .. } => {
                assert_eq!(snapshot.record_id, record);
                assert_eq!(snapshot.metadata, Some(metadata.clone()));
            }
            response => panic!("unexpected response: {response:?}"),
        }

        let mut rng = StdRng::seed_from_u64(123);
        let bearer = mint_bearer_file_invite(
            FileInviteSpec {
                workspace_id: workspace,
                record_id: record,
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
        authority
            .register(&bearer.token, &[issuer_public], 1)
            .unwrap();
        let copied = FileShareLinkV2::bind_at(server.endpoint.addr(), bearer, &issuer, 1).unwrap();
        assert!(matches!(
            client_api
                .request_bearer_link(&copied, RecordProtocolOperationV1::Read)
                .await
                .unwrap(),
            RecordProtocolResponseV1::Snapshot { .. }
        ));

        let denied_view_write = RecordEnvelopeV1::signed(
            record,
            actor,
            2,
            crate::record_replication::RecordOperationV1::SetMetadata(FileMetadataV1 {
                observed_at_ms: 2,
                ..metadata.clone()
            }),
            &client.device,
        )
        .unwrap();
        assert_eq!(
            client_api
                .request_bearer_link(&copied, RecordProtocolOperationV1::Write(denied_view_write))
                .await
                .unwrap(),
            RecordProtocolResponseV1::Denied
        );
        assert_eq!(mutation_sink.calls.lock().unwrap().len(), 1);

        mutation_sink.fail.store(true, Ordering::SeqCst);
        let sink_failure = RecordEnvelopeV1::signed(
            record,
            actor,
            2,
            crate::record_replication::RecordOperationV1::SetMetadata(FileMetadataV1 {
                observed_at_ms: 2,
                ..metadata.clone()
            }),
            &client.device,
        )
        .unwrap();
        assert_eq!(
            client_api
                .request_named(
                    server.endpoint.addr(),
                    token.clone(),
                    RecordProtocolOperationV1::Write(sink_failure.clone())
                )
                .await
                .unwrap(),
            RecordProtocolResponseV1::InternalError
        );
        {
            let calls = mutation_sink.calls.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[1].1, sink_failure);
            drop(calls);
        }

        // The repository commit precedes the live projection. An exact retry
        // of the latest durable envelope must therefore re-run the idempotent
        // sink instead of becoming permanently unrecoverable as a replay.
        mutation_sink.fail.store(false, Ordering::SeqCst);
        assert!(matches!(
            client_api
                .request_named(
                    server.endpoint.addr(),
                    token.clone(),
                    RecordProtocolOperationV1::Write(sink_failure.clone())
                )
                .await
                .unwrap(),
            RecordProtocolResponseV1::Applied { .. }
        ));
        {
            let calls = mutation_sink.calls.lock().unwrap();
            assert_eq!(calls.len(), 3);
            assert_eq!(calls[2].1, sink_failure);
            drop(calls);
        }

        let direct_bearer = mint_bearer_file_invite(
            FileInviteSpec {
                workspace_id: workspace,
                record_id: record,
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
        authority
            .register(&direct_bearer.token, &[issuer_public], 1)
            .unwrap();
        assert!(matches!(
            client_api
                .request_bearer_invite(
                    server.endpoint.addr(),
                    direct_bearer,
                    RecordProtocolOperationV1::Read
                )
                .await
                .unwrap(),
            RecordProtocolResponseV1::Snapshot { .. }
        ));

        let cross = RecordEnvelopeV1::signed(
            other_record,
            actor,
            1,
            crate::record_replication::RecordOperationV1::SetMetadata(FileMetadataV1 {
                schema_version: fs_protocol::files::FILE_METADATA_VERSION,
                record_id: other_record,
                content_hash: None,
                byte_len: 0,
                mime: None,
                previews: vec![],
                latest_actor: Some(actor),
                observed_at_ms: 1,
                index_status: IndexStatus::Pending,
                properties: None,
            }),
            &client.device,
        )
        .unwrap();
        assert_eq!(
            client_api
                .request_named(
                    server.endpoint.addr(),
                    token.clone(),
                    RecordProtocolOperationV1::Write(cross)
                )
                .await
                .unwrap(),
            RecordProtocolResponseV1::Denied
        );
        assert_eq!(mutation_sink.calls.lock().unwrap().len(), 3);
        assert!(!repository.contains(RecordRepositoryKey {
            workspace_id: workspace,
            record_id: other_record,
        }));
        assert!(registry.len() >= 2);

        // A recipient can turn only the exact current signed revision into a
        // durable eviction receipt; the live named share supplies scope and
        // the mutually authenticated connection supplies identity.
        let version = FileVersionV1 {
            id: fs_protocol::FileVersionId::new(),
            record_id: record,
            parents: vec![],
            actor,
            timestamp_ms: unix_now_ms(),
            content_hash: Some(fs_protocol::ContentHash(*replica_manifest.as_bytes())),
            metadata_hash: fs_protocol::ContentHash([44; 32]),
            provenance: fs_protocol::FileProvenance::Edit,
        };
        let revision = ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id: record,
            version_id: version.id,
            content_hash: version.content_hash,
            byte_len: replica_payload.len() as u64,
            metadata_hash: version.metadata_hash,
        };
        for (sequence, operation) in [
            (
                3,
                crate::record_replication::RecordOperationV1::AppendVersion(version),
            ),
            (
                4,
                crate::record_replication::RecordOperationV1::SetContentRevision(revision),
            ),
        ] {
            let envelope =
                RecordEnvelopeV1::signed(record, actor, sequence, operation, &client.device)
                    .unwrap();
            assert!(matches!(
                client_api
                    .request_named(
                        server.endpoint.addr(),
                        token.clone(),
                        RecordProtocolOperationV1::Write(envelope),
                    )
                    .await
                    .unwrap(),
                RecordProtocolResponseV1::Applied { .. }
            ));
        }
        let mut availability_record = crate::availability::RecordAvailability::new(
            record,
            crate::availability::RecordKind::File,
        );
        availability_record.current_version = Some(revision.version_id);
        availability
            .lock()
            .unwrap()
            .put(availability_record)
            .unwrap();
        let request = crate::replica_ack::ReplicaAckRequestV1::new(workspace, revision).unwrap();
        let receipt = ReplicaAckV1::signed(&request, actor, unix_now_ms(), &client.device).unwrap();
        assert!(matches!(
            client_api
                .request_bearer_link(
                    &copied,
                    RecordProtocolOperationV1::AcknowledgeReplica(receipt.clone()),
                )
                .await
                .unwrap(),
            RecordProtocolResponseV1::ReplicaAcknowledged { .. }
        ));
        {
            let persisted = availability.lock().unwrap();
            let availability_record = persisted.get(record).unwrap();
            assert_eq!(availability_record.replicas.durable, 1);
            assert_eq!(
                availability_record.replicas.acknowledged_version,
                Some(revision.version_id)
            );
            assert_eq!(
                availability_record.replica_observations[0].device,
                actor.device
            );
            drop(persisted);
        }
        assert_eq!(
            client_api
                .request_bearer_link(
                    &copied,
                    RecordProtocolOperationV1::AcknowledgeReplica(receipt),
                )
                .await
                .unwrap(),
            RecordProtocolResponseV1::Denied
        );

        // The same read-only exact share can request the reciprocal proof
        // needed to evict its own cached bytes. The receipt is bound to the
        // authenticated host and survives a recipient availability restart.
        // This second recipient has no named edit grant at all.
        let viewer = party().await;
        let viewer_api = RecordProtocolClient::new(
            viewer.endpoint.clone(),
            viewer.credentials.clone(),
            Arc::new(RwLock::new(RevocationList::new())),
        )
        .unwrap();
        let viewer_actor = FileActor {
            person: Ed25519PublicKey::from_bytes(viewer.credentials.user_public_key()),
            device: Ed25519PublicKey::from_bytes(viewer.credentials.device_public_key()),
        };
        let viewer_write = RecordEnvelopeV1::signed(
            record,
            viewer_actor,
            1,
            crate::record_replication::RecordOperationV1::SetMetadata(FileMetadataV1 {
                latest_actor: Some(viewer_actor),
                observed_at_ms: unix_now_ms(),
                ..metadata.clone()
            }),
            &viewer.device,
        )
        .unwrap();
        assert_eq!(
            viewer_api
                .request_bearer_link(&copied, RecordProtocolOperationV1::Write(viewer_write),)
                .await
                .unwrap(),
            RecordProtocolResponseV1::Denied
        );
        let host_request =
            crate::replica_ack::ReplicaAckRequestV1::new(workspace, revision).unwrap();
        let host_observation = viewer_api
            .request_bearer_link_replica_ack(&copied, host_request)
            .await
            .unwrap();
        assert_eq!(
            host_observation.device,
            Ed25519PublicKey(server.credentials.device_public_key())
        );
        assert_eq!(host_observation.record_id, record);
        assert_eq!(host_observation.version_id, revision.version_id);

        let recipient_availability_path = temp.path().join("recipient-availability.json");
        let mut recipient_availability =
            crate::availability::AvailabilityRegistry::open(&recipient_availability_path).unwrap();
        let mut recipient_record = crate::availability::RecordAvailability::new(
            record,
            crate::availability::RecordKind::File,
        );
        recipient_record.current_version = Some(revision.version_id);
        recipient_record.residency = fs_protocol::Residency::Resident {
            bytes: revision.byte_len,
        };
        recipient_availability.put(recipient_record).unwrap();
        let observed_at_ms = host_observation.observed_at_ms;
        recipient_availability
            .observe_replica(
                record,
                viewer_actor.device,
                host_observation,
                observed_at_ms,
            )
            .unwrap();
        assert_eq!(
            recipient_availability.eviction_decision(
                record,
                observed_at_ms,
                crate::availability::EvictionRequirements::default(),
                false,
            ),
            crate::availability::EvictionDecision::Allowed
        );
        drop(recipient_availability);
        let restarted_recipient =
            crate::availability::AvailabilityRegistry::open(recipient_availability_path).unwrap();
        assert_eq!(
            restarted_recipient.eviction_decision(
                record,
                observed_at_ms,
                crate::availability::EvictionRequirements::default(),
                false,
            ),
            crate::availability::EvictionDecision::Allowed
        );

        let wrong_version = ContentRevisionV1 {
            version_id: fs_protocol::FileVersionId::new(),
            ..revision
        };
        assert!(matches!(
            viewer_api
                .request_bearer_link_replica_ack(
                    &copied,
                    crate::replica_ack::ReplicaAckRequestV1::new(workspace, wrong_version).unwrap(),
                )
                .await,
            Err(RecordProtocolClientError::ReplicaDenied)
        ));
        let sibling_revision = ContentRevisionV1 {
            record_id: other_record,
            version_id: fs_protocol::FileVersionId::new(),
            ..revision
        };
        assert!(matches!(
            viewer_api
                .request_bearer_link_replica_ack(
                    &copied,
                    crate::replica_ack::ReplicaAckRequestV1::new(workspace, sibling_revision)
                        .unwrap(),
                )
                .await,
            Err(RecordProtocolClientError::ReplicaDenied)
        ));
        router.shutdown().await.unwrap();
    }

    #[test]
    fn bearer_credentials_are_redacted_and_frames_are_strictly_bounded() {
        let credential = RecordProtocolCredentialV1::BearerInvite {
            invite: mint_file_invite(
                FileInviteSpec {
                    workspace_id: Uuid::new_v4(),
                    record_id: FileRecordId::new(),
                    subject: FileInviteSubject::Person(Ed25519PublicKey::from_bytes([1; 32])),
                    permission: FilePermission::View,
                    expires_at_ms: None,
                    max_redemptions: None,
                },
                &SigningKey::from_bytes(&[9; 32]),
            )
            .unwrap(),
            secret: [0xab; 32],
        };
        let debug = format!("{credential:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&"ab".repeat(32)));
        let request_debug = format!(
            "{:?}",
            RecordProtocolRequestV1 {
                protocol_version: RECORD_PROTOCOL_VERSION,
                credential,
                operation: RecordProtocolOperationV1::Read,
            }
        );
        assert!(request_debug.contains("<redacted>"));
        assert!(!request_debug.contains(&"ab".repeat(32)));
    }

    #[tokio::test]
    async fn frames_are_big_endian_length_then_canonical_bytes() {
        let mut wire = Vec::new();
        write_response(&mut wire, &RecordProtocolResponseV1::Denied)
            .await
            .unwrap();
        assert_eq!(wire, [0, 0, 0, 1, 5]);

        assert!(matches!(
            read_response(&mut &[0, 0, 0, 1, 6][..]).await,
            Ok(RecordProtocolResponseV1::NotFound)
        ));
    }

    #[tokio::test]
    async fn truncated_frames_are_transport_errors() {
        for wire in [&[][..], &[0, 0][..], &[0, 0, 0, 2, 5][..]] {
            assert!(
                matches!(
                    read_response(&mut &wire[..]).await,
                    Err(RecordProtocolClientError::Transport(ref message)) if message == "early eof"
                ),
                "{wire:?}"
            );
            assert!(matches!(
                read_request(&mut &wire[..]).await,
                Err(CanonicalFrameError::Io(_))
            ));
        }
        assert!(matches!(
            read_response(&mut &[0, 0, 0, 0][..]).await,
            Err(RecordProtocolClientError::InvalidResponse)
        ));
    }

    #[tokio::test]
    async fn oversized_or_malformed_frames_are_rejected_without_allocating() {
        {
            use tokio::io::AsyncWriteExt;
            let (mut writer, mut reader) = tokio::io::duplex(16);
            let write = tokio::spawn(async move {
                writer
                    .write_all(
                        &u32::try_from(MAX_RECORD_PROTOCOL_FRAME_BYTES + 1)
                            .unwrap()
                            .to_be_bytes(),
                    )
                    .await
                    .unwrap();
            });
            assert!(matches!(
                read_request(&mut reader).await,
                Err(CanonicalFrameError::TooLarge(_))
            ));
            write.await.unwrap();
        }
        {
            use tokio::io::AsyncWriteExt;
            let (mut writer, mut reader) = tokio::io::duplex(16);
            let write = tokio::spawn(async move {
                writer
                    .write_all(
                        &u32::try_from(MAX_RECORD_PROTOCOL_FRAME_BYTES + 1)
                            .unwrap()
                            .to_be_bytes(),
                    )
                    .await
                    .unwrap();
            });
            assert!(matches!(
                read_response(&mut reader).await,
                Err(RecordProtocolClientError::ResponseTooLarge)
            ));
            write.await.unwrap();
        }
        {
            use tokio::io::AsyncWriteExt;
            let (mut writer, mut reader) = tokio::io::duplex(16);
            let write = tokio::spawn(async move {
                let malformed = [0xff, 0xff];
                writer
                    .write_all(&u32::try_from(malformed.len()).unwrap().to_be_bytes())
                    .await
                    .unwrap();
                writer.write_all(&malformed).await.unwrap();
            });
            assert!(matches!(
                read_response(&mut reader).await,
                Err(RecordProtocolClientError::InvalidResponse)
            ));
            write.await.unwrap();
        }
    }
}
