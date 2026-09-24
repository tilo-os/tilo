//! One authenticated connection per peer for the whole metadata plane.
//!
//! `tilo-fs-session/1` replaces a connection and certified-device handshake
//! per request (namespace exchange, authority, every record-stream pull,
//! every replica receipt) with one connection per peer, authenticated once,
//! carrying each request on its own QUIC stream:
//!
//! - **namespace**: writers run the Automerge sync protocol on one stream;
//!   read-only peers ask for the changes since their heads instead of
//!   downloading the whole document;
//! - **records**: range-based reconciliation of stream heads
//!   ([`crate::record_reconcile`]) finds the streams that differ in a few
//!   round trips however large the workspace, and one pull fetches the
//!   missing envelopes of up to [`MAX_PULL_STREAMS`] streams, with the
//!   content of small files inline;
//! - **receipts**: replica acknowledgements for many records per request;
//! - **authority**: the signed workspace ACL exchange;
//! - **announce**: a peer that published new changes tells its peers, which
//!   pull at once instead of at their next reconciliation tick.
//!
//! Every request is authorised on its own against the live policy and the
//! peer's live certification, exactly as the per-request protocols decide;
//! only the handshake is shared. Peers that do not speak this protocol are
//! served and reached through the per-request protocols, which stay
//! registered unchanged.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use bytes::Bytes;
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh_base::EndpointAddr;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::{
    DirectMeshAuthorizer, FsTransportIdentity, PeerPolicy, RecordPlane, authenticate_connection,
    authenticated_peer_may_read_whole_workspace, authenticated_peer_may_write_whole_workspace,
    unix_now_ms, unix_now_ns,
};
use crate::hydration::Hydrator;
use crate::namespace::Namespace;
use crate::peer_auth::AuthenticatedFsPeer;
use crate::record_mesh::{RecordMeshErrorCodeV1, RecordMeshStreamV1};
use crate::record_reconcile::{self, ReconcileAnswerV1, ReconcileRangeV1};
use crate::record_replication::{RecordEnvelopeV1, RecordOperationV1};
use crate::record_repository::{
    MAX_RECORD_BATCH_OPERATIONS, RecordRepositoryKey, RecordStreamCursor,
};
use crate::replica_ack::{ReplicaAckRequestV1, ReplicaAckResponseV1};
use crate::share_session::LiveActorCertification;
use crate::store::ContentStore;
use crate::workspace_authority::{AuthorityRequest, AuthorityResponse, WorkspaceAuthority};

pub const TILO_FS_SESSION_ALPN: &[u8] = b"tilo-fs-session/1";
pub const SESSION_PROTOCOL_VERSION: u16 = 1;

/// Largest request or response frame, except Automerge sync messages
/// between writers, which carry whole changes like the per-request sync
/// protocol does.
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_SYNC_FRAME_BYTES: usize = 64 * 1024 * 1024;
/// A namespace delta for a read-only peer may be as large as the document.
const MAX_NAMESPACE_BYTES: usize = 64 * 1024 * 1024;
/// Response frames are cut at about this size.
const TARGET_FRAME_BYTES: usize = 1024 * 1024;
/// Ranges in one reconciliation request.
pub const MAX_RECONCILE_RANGES: usize = 256;
/// Streams in one pull request.
pub const MAX_PULL_STREAMS: usize = 1024;
/// Receipts in one request.
pub const MAX_REPLICA_ACKS: usize = 256;
/// A file up to this size travels inside the pull that carries its record.
pub const INLINE_CONTENT_BYTES: u32 = 4 * 1024;
/// Requests one connection may have in progress.
const MAX_STREAMS_PER_CONNECTION: usize = 16;
/// Automerge sync rounds on one stream, as in the per-request protocol.
const MAX_SYNC_ROUNDS: usize = 200;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a peer that does not speak this protocol is reached through
/// the per-request protocols before trying again.
const LEGACY_RETRY: Duration = Duration::from_secs(300);
/// A session quiet for longer than this is pinged before use.
const PING_AFTER: Duration = Duration::from_secs(2);
const PING_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRequestV1 {
    pub protocol_version: u16,
    pub workspace_id: uuid::Uuid,
    pub operation: SessionOperationV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum SessionOperationV1 {
    /// Compare record stream heads range by range.
    Reconcile {
        ranges: Vec<ReconcileRangeV1>,
    },
    /// The envelopes after each cursor, and the bytes of files up to
    /// `inline_content_bytes` that the returned envelopes make current.
    Pull {
        streams: Vec<SessionPullV1>,
        inline_content_bytes: u32,
    },
    /// Automerge sync rounds between two workspace writers follow on the
    /// stream as [`SessionFrameV1::Sync`] frames, requester first.
    NamespaceSync,
    /// The namespace changes beyond `heads`, for a read-only peer.
    NamespaceSince {
        heads: Vec<[u8; 32]>,
    },
    ReplicaAcks {
        requests: Vec<ReplicaAckRequestV1>,
    },
    Authority(AuthorityRequest),
    /// The requester published changes; pull from it now.
    Announce,
    /// Answered with `Done`: the connection still reaches the peer.
    Ping,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionPullV1 {
    pub stream: RecordMeshStreamV1,
    /// The requester's prefix of the stream; `None` is the empty prefix.
    pub after: Option<RecordStreamCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEnvelopesV1 {
    pub stream: RecordMeshStreamV1,
    pub after_sequence: u64,
    pub envelopes: Vec<RecordEnvelopeV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionBlobV1 {
    pub hash: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::large_enum_variant,
    reason = "frames are decoded one at a time; boxing would change nothing but the wire-facing type"
)]
pub enum SessionFrameV1 {
    /// One answer per requested range, in order.
    Reconciled {
        answers: Vec<ReconcileAnswerV1>,
    },
    Envelopes {
        batches: Vec<SessionEnvelopesV1>,
    },
    StreamFailed {
        stream: RecordMeshStreamV1,
        code: RecordMeshErrorCodeV1,
    },
    Blobs {
        blobs: Vec<SessionBlobV1>,
    },
    Sync(#[serde(with = "serde_bytes")] Vec<u8>),
    NamespaceChunk(#[serde(with = "serde_bytes")] Vec<u8>),
    ReplicaAcks {
        responses: Vec<ReplicaAckResponseV1>,
    },
    Authority(AuthorityResponse),
    /// The response is complete.
    Done,
    Denied,
    Error(RecordMeshErrorCodeV1),
}

async fn write_frame<T: Serialize + Sync>(send: &mut SendStream, value: &T) -> Result<()> {
    let bytes = postcard::to_allocvec(value)?;
    let length = u32::try_from(bytes.len()).map_err(|_| anyhow!("session frame too large"))?;
    send.write_all(&length.to_be_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

async fn read_frame<T: serde::de::DeserializeOwned>(
    recv: &mut RecvStream,
    limit: usize,
) -> Result<T> {
    let mut prefix = [0_u8; 4];
    recv.read_exact(&mut prefix).await?;
    let length = u32::from_be_bytes(prefix) as usize;
    anyhow::ensure!(
        length <= limit,
        "session frame of {length} bytes exceeds {limit}"
    );
    let mut bytes = vec![0_u8; length];
    recv.read_exact(&mut bytes).await?;
    Ok(postcard::from_bytes(&bytes)?)
}

fn denied(peer: &AuthenticatedFsPeer, identity: &FsTransportIdentity) -> bool {
    identity.partitioned() || !identity.certification_is_current(peer, unix_now_ns())
}

// ---------------------------------------------------------------------------
// Serving

/// The server side of the session protocol for one workspace.
#[derive(Clone)]
pub(super) struct SessionProtocol {
    pub endpoint: Endpoint,
    pub identity: FsTransportIdentity,
    pub policy: PeerPolicy,
    pub namespace: Arc<Mutex<Namespace>>,
    pub store: Arc<ContentStore>,
    pub records: Option<RecordPlane>,
    pub workspace_id: uuid::Uuid,
    /// Peers that announced changes, for the reconciliation loop.
    pub announcements: mpsc::UnboundedSender<iroh_base::PublicKey>,
}

impl std::fmt::Debug for SessionProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionProtocol")
    }
}

impl ProtocolHandler for SessionProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if self.identity.partitioned() {
            connection.close(1u32.into(), b"partitioned");
            return Ok(());
        }
        let handshake = async {
            let (mut send, mut recv) = connection.accept_bi().await?;
            let peer = authenticate_connection(
                &self.endpoint,
                &connection,
                &mut recv,
                &mut send,
                &self.identity,
            )
            .await?;
            send.finish()?;
            Ok::<_, anyhow::Error>(peer)
        };
        let peer = match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
            Ok(Ok(peer)) => peer,
            Ok(Err(error)) => {
                return Err(AcceptError::from_err(std::io::Error::other(
                    error.to_string(),
                )));
            }
            Err(_) => return Ok(()),
        };
        let slots = Arc::new(tokio::sync::Semaphore::new(MAX_STREAMS_PER_CONNECTION));
        while let Ok((send, recv)) = connection.accept_bi().await {
            let Ok(slot) = Arc::clone(&slots).acquire_owned().await else {
                break;
            };
            let this = self.clone();
            let peer = peer.clone();
            let remote = connection.remote_id();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(
                    REQUEST_TIMEOUT,
                    this.serve_stream(&peer, remote, send, recv),
                )
                .await;
                drop(slot);
            });
        }
        Ok(())
    }
}

impl SessionProtocol {
    async fn serve_stream(
        &self,
        peer: &AuthenticatedFsPeer,
        remote: iroh_base::PublicKey,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> Result<()> {
        let request: SessionRequestV1 = read_frame(&mut recv, MAX_FRAME_BYTES).await?;
        if denied(peer, &self.identity) {
            write_frame(&mut send, &SessionFrameV1::Denied).await?;
            send.finish()?;
            return Ok(());
        }
        if request.protocol_version != SESSION_PROTOCOL_VERSION
            || request.workspace_id != self.workspace_id
        {
            let code = if request.protocol_version == SESSION_PROTOCOL_VERSION {
                RecordMeshErrorCodeV1::InvalidRequest
            } else {
                RecordMeshErrorCodeV1::UnsupportedVersion
            };
            write_frame(&mut send, &SessionFrameV1::Error(code)).await?;
            send.finish()?;
            return Ok(());
        }
        match request.operation {
            SessionOperationV1::Reconcile { ranges } => {
                self.serve_reconcile(peer, &ranges, &mut send).await?;
            }
            SessionOperationV1::Pull {
                streams,
                inline_content_bytes,
            } => {
                self.serve_pull(peer, &streams, inline_content_bytes, &mut send)
                    .await?;
            }
            SessionOperationV1::NamespaceSync => {
                self.serve_namespace_sync(peer, &mut send, &mut recv)
                    .await?;
            }
            SessionOperationV1::NamespaceSince { heads } => {
                self.serve_namespace_since(peer, &heads, &mut send).await?;
            }
            SessionOperationV1::ReplicaAcks { requests } => {
                self.serve_replica_acks(peer, &requests, &mut send).await?;
            }
            SessionOperationV1::Authority(request) => {
                self.serve_authority(peer, request, &mut send).await?;
            }
            SessionOperationV1::Announce => {
                // Only a peer this device pulls from (a workspace reader) may
                // start a pass early; anyone else waits for the timer.
                if authenticated_peer_may_read_whole_workspace(&self.policy, peer) {
                    let _ = self.announcements.send(remote);
                }
                write_frame(&mut send, &SessionFrameV1::Done).await?;
            }
            SessionOperationV1::Ping => {
                write_frame(&mut send, &SessionFrameV1::Done).await?;
            }
        }
        send.finish()?;
        Ok(())
    }

    /// The record plane, if the peer may read the whole workspace (the
    /// condition of the per-request record mesh).
    fn readable_records(&self, peer: &AuthenticatedFsPeer) -> Option<&RecordPlane> {
        self.records
            .as_ref()
            .filter(|_| authenticated_peer_may_read_whole_workspace(&self.policy, peer))
    }

    async fn serve_reconcile(
        &self,
        peer: &AuthenticatedFsPeer,
        ranges: &[ReconcileRangeV1],
        send: &mut SendStream,
    ) -> Result<()> {
        let Some(records) = self.readable_records(peer) else {
            return write_frame(send, &SessionFrameV1::Denied).await;
        };
        if ranges.is_empty() || ranges.len() > MAX_RECONCILE_RANGES {
            return write_frame(
                send,
                &SessionFrameV1::Error(RecordMeshErrorCodeV1::RequestTooLarge),
            )
            .await;
        }
        // Peers pull the streams this device's person authored, as the
        // per-request mesh does: the transport certifies only that person.
        let person = Some(self.identity.user_public_key());
        let mut answers = Vec::with_capacity(ranges.len());
        for range in ranges {
            let items = records.repository.stream_cursors_in(
                records.workspace_id,
                range.lower.as_ref().map(record_reconcile::stream_key),
                range.upper.as_ref().map(record_reconcile::stream_key),
                person,
            );
            let Ok(items) = items else {
                return write_frame(
                    send,
                    &SessionFrameV1::Error(RecordMeshErrorCodeV1::Internal),
                )
                .await;
            };
            answers.push(record_reconcile::answer(range, &items));
        }
        // Answers are bounded by the item thresholds; split them over
        // frames so no frame outgrows the limit.
        let mut frame = Vec::new();
        let mut size = 0_usize;
        for answer in answers {
            let answer_size = match &answer {
                ReconcileAnswerV1::Items(items) => items.len().saturating_mul(160),
                ReconcileAnswerV1::Split(parts) => parts.len().saturating_mul(200),
                ReconcileAnswerV1::Equal => 1,
            };
            if !frame.is_empty() && size.saturating_add(answer_size) > TARGET_FRAME_BYTES {
                write_frame(
                    send,
                    &SessionFrameV1::Reconciled {
                        answers: std::mem::take(&mut frame),
                    },
                )
                .await?;
                size = 0;
            }
            size = size.saturating_add(answer_size);
            frame.push(answer);
        }
        write_frame(send, &SessionFrameV1::Reconciled { answers: frame }).await?;
        write_frame(send, &SessionFrameV1::Done).await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one pull validates, batches and inlines in a single pass over the requested streams"
    )]
    async fn serve_pull(
        &self,
        peer: &AuthenticatedFsPeer,
        streams: &[SessionPullV1],
        inline_content_bytes: u32,
        send: &mut SendStream,
    ) -> Result<()> {
        let Some(records) = self.readable_records(peer) else {
            return write_frame(send, &SessionFrameV1::Denied).await;
        };
        if streams.is_empty() || streams.len() > MAX_PULL_STREAMS {
            return write_frame(
                send,
                &SessionFrameV1::Error(RecordMeshErrorCodeV1::RequestTooLarge),
            )
            .await;
        }
        let inline_limit = u64::from(inline_content_bytes.min(INLINE_CONTENT_BYTES));
        let mut batches = Vec::new();
        let mut size = 0_usize;
        // The content each pulled record ends at, per record.
        let mut current: BTreeMap<collab_types::FileRecordId, [u8; 32]> = BTreeMap::new();
        for pull in streams {
            let stream = pull.stream;
            let key = RecordRepositoryKey {
                workspace_id: records.workspace_id,
                record_id: stream.record_id,
            };
            let failed = |code| SessionFrameV1::StreamFailed { stream, code };
            if pull.after.as_ref().is_some_and(|cursor| {
                cursor.record_id != stream.record_id || cursor.actor != stream.actor
            }) {
                write_frame(send, &failed(RecordMeshErrorCodeV1::InvalidRequest)).await?;
                continue;
            }
            let mut after = pull.after.as_ref().map_or(0, |c| c.contiguous_sequence);
            if let Some(cursor) = &pull.after {
                match records.repository.stream_cursor(
                    key,
                    &stream.actor,
                    cursor.contiguous_sequence,
                ) {
                    Ok(local) if local == *cursor => {}
                    Ok(_) => {
                        write_frame(send, &failed(RecordMeshErrorCodeV1::PrefixMismatch)).await?;
                        continue;
                    }
                    Err(_) => {
                        write_frame(send, &failed(RecordMeshErrorCodeV1::InvalidRequest)).await?;
                        continue;
                    }
                }
            }
            loop {
                let Ok(envelopes) = records.repository.envelopes_after(
                    key,
                    &stream.actor,
                    after,
                    MAX_RECORD_BATCH_OPERATIONS,
                ) else {
                    write_frame(send, &failed(RecordMeshErrorCodeV1::InvalidRequest)).await?;
                    break;
                };
                let Some(last) = envelopes.last() else {
                    break;
                };
                let next_after = last.sequence;
                for envelope in &envelopes {
                    size = size.saturating_add(
                        envelope
                            .canonical_bytes()
                            .map_or(TARGET_FRAME_BYTES, |bytes| bytes.len()),
                    );
                    if let RecordOperationV1::SetContentRevision(revision) = &envelope.operation {
                        match revision.content_hash {
                            Some(hash) if revision.byte_len <= inline_limit => {
                                current.insert(revision.record_id, hash.0);
                            }
                            _ => {
                                current.remove(&revision.record_id);
                            }
                        }
                    }
                }
                let full = envelopes.len() == MAX_RECORD_BATCH_OPERATIONS;
                batches.push(SessionEnvelopesV1 {
                    stream,
                    after_sequence: after,
                    envelopes,
                });
                after = next_after;
                if size >= TARGET_FRAME_BYTES {
                    write_frame(
                        send,
                        &SessionFrameV1::Envelopes {
                            batches: std::mem::take(&mut batches),
                        },
                    )
                    .await?;
                    size = 0;
                }
                if !full {
                    break;
                }
            }
        }
        if !batches.is_empty() {
            write_frame(send, &SessionFrameV1::Envelopes { batches }).await?;
        }
        // Inline content: the requester may read every record it was just
        // sent (whole-workspace read), so it may read their current bytes.
        let mut blobs = Vec::new();
        let mut size = 0_usize;
        let mut inlined = HashSet::new();
        for hash in current.into_values() {
            for blob in self
                .inline_blobs(iroh_blobs::Hash::from(hash), inline_limit)
                .await
            {
                if !inlined.insert(blob.hash) {
                    continue;
                }
                size = size.saturating_add(blob.bytes.len());
                blobs.push(blob);
                if size >= TARGET_FRAME_BYTES {
                    write_frame(
                        send,
                        &SessionFrameV1::Blobs {
                            blobs: std::mem::take(&mut blobs),
                        },
                    )
                    .await?;
                    size = 0;
                }
            }
        }
        if !blobs.is_empty() {
            write_frame(send, &SessionFrameV1::Blobs { blobs }).await?;
        }
        write_frame(send, &SessionFrameV1::Done).await
    }

    /// The manifest and chunks of a small file, when all are local.
    async fn inline_blobs(&self, manifest: iroh_blobs::Hash, limit: u64) -> Vec<SessionBlobV1> {
        let Some(bytes) = self.store.local(manifest).await else {
            return Vec::new();
        };
        let Ok(decoded) = crate::content::Manifest::decode(&bytes) else {
            return Vec::new();
        };
        let total: u64 = decoded.chunks.iter().map(|c| u64::from(c.len)).sum();
        if total > limit {
            return Vec::new();
        }
        let mut out = vec![SessionBlobV1 {
            hash: *manifest.as_bytes(),
            bytes: bytes.to_vec(),
        }];
        for chunk in &decoded.chunks {
            let Some(data) = self.store.local(iroh_blobs::Hash::from(chunk.hash)).await else {
                return Vec::new();
            };
            out.push(SessionBlobV1 {
                hash: chunk.hash,
                bytes: data.to_vec(),
            });
        }
        out
    }

    async fn serve_namespace_sync(
        &self,
        peer: &AuthenticatedFsPeer,
        send: &mut SendStream,
        recv: &mut RecvStream,
    ) -> Result<()> {
        if !authenticated_peer_may_write_whole_workspace(&self.policy, peer) {
            return write_frame(send, &SessionFrameV1::Denied).await;
        }
        write_frame(send, &SessionFrameV1::Done).await?;
        let mut state = automerge::sync::State::new();
        for _ in 0..MAX_SYNC_ROUNDS {
            // The authority handle is live: a revocation cuts a connected
            // writer off at the next round.
            if !authenticated_peer_may_write_whole_workspace(&self.policy, peer)
                || denied(peer, &self.identity)
            {
                break;
            }
            let SessionFrameV1::Sync(message) = read_frame(recv, MAX_SYNC_FRAME_BYTES).await?
            else {
                break;
            };
            if !message.is_empty() {
                let _ = self
                    .namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .sync_receive(&mut state, &message);
            }
            let reply = self
                .namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sync_generate(&mut state);
            let done = message.is_empty() && reply.is_none();
            write_frame(send, &SessionFrameV1::Sync(reply.unwrap_or_default())).await?;
            if done {
                break;
            }
        }
        Ok(())
    }

    async fn serve_namespace_since(
        &self,
        peer: &AuthenticatedFsPeer,
        heads: &[[u8; 32]],
        send: &mut SendStream,
    ) -> Result<()> {
        if !authenticated_peer_may_read_whole_workspace(&self.policy, peer) || heads.len() > 1024 {
            return write_frame(send, &SessionFrameV1::Denied).await;
        }
        let heads: Vec<automerge::ChangeHash> =
            heads.iter().map(|h| automerge::ChangeHash(*h)).collect();
        let delta = self
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .changes_since(&heads);
        for chunk in delta.chunks(TARGET_FRAME_BYTES) {
            write_frame(send, &SessionFrameV1::NamespaceChunk(chunk.to_vec())).await?;
        }
        write_frame(send, &SessionFrameV1::Done).await
    }

    async fn serve_replica_acks(
        &self,
        peer: &AuthenticatedFsPeer,
        requests: &[ReplicaAckRequestV1],
        send: &mut SendStream,
    ) -> Result<()> {
        let Some(records) = &self.records else {
            return write_frame(send, &SessionFrameV1::Denied).await;
        };
        if requests.len() > MAX_REPLICA_ACKS {
            return write_frame(
                send,
                &SessionFrameV1::Error(RecordMeshErrorCodeV1::RequestTooLarge),
            )
            .await;
        }
        let mut responses = Vec::with_capacity(requests.len());
        for request in requests {
            responses.push(
                super::replica_ack_response(
                    &self.identity,
                    &self.policy,
                    records.workspace_id,
                    &records.repository,
                    &self.store,
                    peer,
                    request,
                )
                .await,
            );
        }
        write_frame(send, &SessionFrameV1::ReplicaAcks { responses }).await?;
        write_frame(send, &SessionFrameV1::Done).await
    }

    async fn serve_authority(
        &self,
        peer: &AuthenticatedFsPeer,
        request: AuthorityRequest,
        send: &mut SendStream,
    ) -> Result<()> {
        let Some(authority) = self
            .records
            .as_ref()
            .and_then(|records| records.workspace_authority.as_ref())
        else {
            // This device holds no signed authority (only its governor's
            // mount does): there is nothing to give or take.
            write_frame(
                send,
                &SessionFrameV1::Authority(AuthorityResponse { authority: None }),
            )
            .await?;
            return write_frame(send, &SessionFrameV1::Done).await;
        };
        let response =
            super::authority_response(authority, &self.policy, self.workspace_id, peer, request);
        write_frame(send, &SessionFrameV1::Authority(response)).await?;
        write_frame(send, &SessionFrameV1::Done).await
    }
}

// ---------------------------------------------------------------------------
// Requesting

/// One authenticated connection to a peer.
pub(super) struct Session {
    connection: Connection,
    pub peer: AuthenticatedFsPeer,
    pub addr: EndpointAddr,
    /// When a response last arrived on this session.
    last_heard: Mutex<Instant>,
}

/// Sessions to every peer this device reconciles with, opened on first use.
pub(super) struct SessionPool {
    endpoint: Endpoint,
    identity: FsTransportIdentity,
    workspace_id: uuid::Uuid,
    sessions: tokio::sync::Mutex<HashMap<iroh_base::PublicKey, Arc<Session>>>,
    legacy: Mutex<HashMap<iroh_base::PublicKey, Instant>>,
}

/// Whether a connection attempt failed because the peer does not offer the
/// protocol (TLS `no_application_protocol`), as opposed to being away.
fn protocol_unsupported(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}");
    text.contains("peer doesn't support any known protocol")
        || text.contains("no_application_protocol")
        || text.contains("no application protocol")
}

impl SessionPool {
    pub(super) fn new(
        endpoint: Endpoint,
        identity: FsTransportIdentity,
        workspace_id: uuid::Uuid,
    ) -> Self {
        Self {
            endpoint,
            identity,
            workspace_id,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            legacy: Mutex::new(HashMap::new()),
        }
    }

    /// The live session to `addr`, opening one if needed. `Ok(None)` means
    /// the peer only speaks the per-request protocols.
    pub(super) async fn get(&self, addr: &EndpointAddr) -> Result<Option<Arc<Session>>> {
        if self.identity.partitioned() {
            anyhow::bail!("transport partition gate is closed");
        }
        if let Some(session) = self.live(addr.id).await {
            // A peer that restarted leaves a connection that looks open until
            // QUIC's idle timeout; a quiet session proves itself first.
            if session.heard_within(PING_AFTER) || self.ping(&session).await {
                return Ok(Some(session));
            }
            session.connection.close(0u32.into(), b"unresponsive");
            self.forget(&session).await;
        }
        if self
            .legacy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&addr.id)
            .is_some_and(|until| Instant::now() < *until)
        {
            return Ok(None);
        }
        let opened = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let connection = self
                .endpoint
                .connect(addr.clone(), TILO_FS_SESSION_ALPN)
                .await?;
            let (mut send, mut recv) = connection.open_bi().await?;
            let peer = authenticate_connection(
                &self.endpoint,
                &connection,
                &mut recv,
                &mut send,
                &self.identity,
            )
            .await?;
            send.finish()?;
            Ok::<_, anyhow::Error>((connection, peer))
        })
        .await
        .map_err(|_| anyhow!("opening a session to {} timed out", addr.id))?;
        match opened {
            Ok((connection, peer)) => {
                let session = Arc::new(Session {
                    connection,
                    peer,
                    addr: addr.clone(),
                    last_heard: Mutex::new(Instant::now()),
                });
                // Another caller may have opened one meanwhile; keep one.
                let mut sessions = self.sessions.lock().await;
                let kept = match sessions.get(&addr.id) {
                    Some(existing) if existing.connection.close_reason().is_none() => {
                        session.connection.close(0u32.into(), b"duplicate");
                        Arc::clone(existing)
                    }
                    _ => {
                        sessions.insert(addr.id, Arc::clone(&session));
                        session
                    }
                };
                drop(sessions);
                Ok(Some(kept))
            }
            Err(error) if protocol_unsupported(&error) => {
                if let Some(until) = Instant::now().checked_add(LEGACY_RETRY) {
                    self.legacy
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(addr.id, until);
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// The pooled session to `id` if its connection is open and the peer's
    /// certification still current; a stale one is dropped.
    async fn live(&self, id: iroh_base::PublicKey) -> Option<Arc<Session>> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get(&id)?;
        if session.connection.close_reason().is_none()
            && self
                .identity
                .certification_is_current(&session.peer, unix_now_ns())
        {
            return Some(Arc::clone(session));
        }
        sessions.remove(&id);
        None
    }

    /// Drops `session` after a transport failure; the next use reconnects.
    pub(super) async fn forget(&self, session: &Session) {
        let mut sessions = self.sessions.lock().await;
        if sessions
            .get(&session.addr.id)
            .is_some_and(|s| s.connection.stable_id() == session.connection.stable_id())
        {
            sessions.remove(&session.addr.id);
        }
    }

    /// Every open session.
    pub(super) async fn open_sessions(&self) -> Vec<Arc<Session>> {
        self.sessions
            .lock()
            .await
            .values()
            .filter(|s| s.connection.close_reason().is_none())
            .cloned()
            .collect()
    }

    /// Whether the peer answers on `session` promptly.
    async fn ping(&self, session: &Session) -> bool {
        let exchange = async {
            let (mut send, mut recv) = session
                .open(&self.request(SessionOperationV1::Ping))
                .await?;
            let _ = send.finish();
            while session.frames(&mut recv, MAX_FRAME_BYTES).await?.is_some() {}
            Ok::<_, anyhow::Error>(())
        };
        matches!(
            tokio::time::timeout(PING_TIMEOUT, exchange).await,
            Ok(Ok(()))
        )
    }

    fn request(&self, operation: SessionOperationV1) -> SessionRequestV1 {
        SessionRequestV1 {
            protocol_version: SESSION_PROTOCOL_VERSION,
            workspace_id: self.workspace_id,
            operation,
        }
    }
}

impl Session {
    /// Whether the connection is gone (the pool then opens a new one).
    pub(super) fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }

    /// Sends `request` on a new stream and returns the stream for its
    /// response frames.
    async fn open(&self, request: &SessionRequestV1) -> Result<(SendStream, RecvStream)> {
        let (mut send, recv) = self.connection.open_bi().await?;
        write_frame(&mut send, request).await?;
        Ok((send, recv))
    }

    fn heard_within(&self, window: Duration) -> bool {
        self.last_heard
            .lock()
            .is_ok_and(|heard| heard.elapsed() < window)
    }

    fn heard(&self) {
        if let Ok(mut heard) = self.last_heard.lock() {
            *heard = Instant::now();
        }
    }

    /// Frames up to `Done`. `Denied` and `Error` become errors.
    async fn frames(&self, recv: &mut RecvStream, limit: usize) -> Result<Option<SessionFrameV1>> {
        let frame = read_frame(recv, limit).await?;
        self.heard();
        match frame {
            SessionFrameV1::Done => Ok(None),
            SessionFrameV1::Denied => Err(anyhow!("session request denied")),
            SessionFrameV1::Error(code) => Err(anyhow!("session request failed: {code:?}")),
            frame => Ok(Some(frame)),
        }
    }
}

/// Automerge sync with a workspace writer on one stream. Returns whether
/// changes were received.
pub(super) async fn sync_namespace(
    pool: &SessionPool,
    session: &Session,
    namespace: &Arc<Mutex<Namespace>>,
) -> Result<bool> {
    let (mut send, mut recv) = session
        .open(&pool.request(SessionOperationV1::NamespaceSync))
        .await?;
    if session.frames(&mut recv, MAX_FRAME_BYTES).await?.is_some() {
        anyhow::bail!("unexpected namespace sync response");
    }
    let mut state = automerge::sync::State::new();
    let mut received = false;
    for _ in 0..MAX_SYNC_ROUNDS {
        let message = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sync_generate(&mut state);
        let sent_nothing = message.is_none();
        write_frame(
            &mut send,
            &SessionFrameV1::Sync(message.unwrap_or_default()),
        )
        .await?;
        let SessionFrameV1::Sync(reply) = read_frame(&mut recv, MAX_SYNC_FRAME_BYTES).await? else {
            anyhow::bail!("unexpected namespace sync frame");
        };
        session.heard();
        if !reply.is_empty() {
            received = true;
            let _ = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sync_receive(&mut state, &reply);
        }
        if sent_nothing && reply.is_empty() {
            break;
        }
    }
    let _ = send.finish();
    Ok(received)
}

/// The namespace changes a peer holds beyond this device's heads, for a
/// read-only device. Returns the resolution delta and whether anything was
/// applied.
pub(super) async fn pull_namespace(
    pool: &SessionPool,
    session: &Session,
    namespace: &Arc<Mutex<Namespace>>,
) -> Result<(bool, Vec<u8>)> {
    let heads = namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .heads()
        .into_iter()
        .map(|h| h.0)
        .collect();
    let (mut send, mut recv) = session
        .open(&pool.request(SessionOperationV1::NamespaceSince { heads }))
        .await?;
    let _ = send.finish();
    let mut delta = Vec::new();
    while let Some(frame) = session.frames(&mut recv, MAX_FRAME_BYTES).await? {
        let SessionFrameV1::NamespaceChunk(chunk) = frame else {
            anyhow::bail!("unexpected namespace frame");
        };
        anyhow::ensure!(
            delta.len().saturating_add(chunk.len()) <= MAX_NAMESPACE_BYTES,
            "namespace delta exceeds {MAX_NAMESPACE_BYTES} bytes"
        );
        delta.extend_from_slice(&chunk);
    }
    if delta.is_empty() {
        return Ok((false, Vec::new()));
    }
    let mut ns = namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = ns.heads();
    let resolution = ns.apply_remote_and_resolve(&delta).unwrap_or_default();
    let changed = ns.heads() != before;
    drop(ns);
    Ok((changed, resolution))
}

/// Exchanges the signed workspace authority, publishing this device's when
/// it is the governor.
pub(super) async fn exchange_authority(
    pool: &SessionPool,
    session: &Session,
    identity: &FsTransportIdentity,
    authority: &WorkspaceAuthority,
    workspace_id: uuid::Uuid,
) -> Result<()> {
    let request = AuthorityRequest {
        workspace: workspace_id,
        publish: (identity.user_public_key() == authority.governor())
            .then(|| authority.current())
            .flatten(),
    };
    let (mut send, mut recv) = session
        .open(&pool.request(SessionOperationV1::Authority(request)))
        .await?;
    let _ = send.finish();
    while let Some(frame) = session.frames(&mut recv, MAX_FRAME_BYTES).await? {
        if let SessionFrameV1::Authority(response) = frame
            && let Some(envelope) = response.authority
        {
            authority.apply(envelope)?;
        }
    }
    Ok(())
}

/// Tells the peer this device published changes.
pub(super) async fn announce(pool: &SessionPool, session: &Session) -> Result<()> {
    let (mut send, mut recv) = session
        .open(&pool.request(SessionOperationV1::Announce))
        .await?;
    let _ = send.finish();
    while session.frames(&mut recv, MAX_FRAME_BYTES).await?.is_some() {}
    Ok(())
}

/// The streams whose heads differ from the peer's, by range-based
/// reconciliation over the streams the peer's person authored.
async fn reconcile_streams(
    pool: &SessionPool,
    session: &Session,
    records: &RecordPlane,
) -> Result<Vec<RecordStreamCursor>> {
    let person = session.peer.user_public_key;
    let local = |range: Option<&ReconcileRangeV1>| {
        records.repository.stream_cursors_in(
            records.workspace_id,
            range
                .and_then(|r| r.lower.as_ref())
                .map(record_reconcile::stream_key),
            range
                .and_then(|r| r.upper.as_ref())
                .map(record_reconcile::stream_key),
            Some(person),
        )
    };
    let mut ranges = vec![record_reconcile::summarize(None, None, &local(None)?)];
    let mut learned = Vec::new();
    // Each round descends one level; the depth is logarithmic in the
    // workspace, so this bound is never reached by an honest peer.
    for _ in 0..16 {
        if ranges.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for chunk in ranges.chunks(MAX_RECONCILE_RANGES) {
            let (mut send, mut recv) = session
                .open(&pool.request(SessionOperationV1::Reconcile {
                    ranges: chunk.to_vec(),
                }))
                .await?;
            let _ = send.finish();
            let mut answers = Vec::with_capacity(chunk.len());
            while let Some(frame) = session.frames(&mut recv, MAX_FRAME_BYTES).await? {
                let SessionFrameV1::Reconciled { answers: part } = frame else {
                    anyhow::bail!("unexpected reconcile frame");
                };
                answers.extend(part);
            }
            anyhow::ensure!(
                answers.len() == chunk.len(),
                "reconcile answered {} of {} ranges",
                answers.len(),
                chunk.len()
            );
            for (range, answer) in chunk.iter().zip(answers) {
                match answer {
                    ReconcileAnswerV1::Equal => {}
                    ReconcileAnswerV1::Items(items) => learned.extend(
                        items
                            .into_iter()
                            .filter(|item| record_reconcile::contains(range, item)),
                    ),
                    ReconcileAnswerV1::Split(parts) => {
                        for part in parts.into_iter().take(record_reconcile::SPLIT_PARTS) {
                            let mine = local(Some(&part))?;
                            let summary =
                                record_reconcile::summarize(part.lower, part.upper, &mine);
                            if (summary.fingerprint, summary.count)
                                != (part.fingerprint, part.count)
                            {
                                next.push(summary);
                            }
                        }
                    }
                }
            }
        }
        ranges = next;
    }
    Ok(learned)
}

/// Pulls every record stream the peer is ahead on: reconciliation, then
/// batched pulls of many streams each, with small files' content inline.
/// Returns whether anything was applied. With `heal`, every record is
/// re-projected even when nothing new arrived.
#[allow(
    clippy::too_many_lines,
    reason = "the pull validates, applies and materialises one ordered response stream"
)]
pub(super) async fn pull_records(
    pool: &SessionPool,
    session: &Session,
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    records: &RecordPlane,
    hydrator: &Hydrator,
    heal: bool,
) -> Result<bool> {
    let remote = reconcile_streams(pool, session, records).await?;
    let mut pulls = Vec::new();
    for cursor in remote {
        if cursor.actor.person.as_bytes() != &session.peer.user_public_key {
            continue;
        }
        let key = RecordRepositoryKey {
            workspace_id: records.workspace_id,
            record_id: cursor.record_id,
        };
        let local = records
            .repository
            .highest_sequence(key, &cursor.actor)
            .unwrap_or(0);
        if local > cursor.contiguous_sequence {
            continue;
        }
        let after = if local == 0 {
            None
        } else {
            Some(
                records
                    .repository
                    .stream_cursor(key, &cursor.actor, local)?,
            )
        };
        if local == cursor.contiguous_sequence {
            if after.is_some_and(|after| after != cursor) {
                eprintln!(
                    "record stream {:?} diverged from {}",
                    cursor.record_id, session.addr.id
                );
            }
            continue;
        }
        pulls.push(SessionPullV1 {
            stream: RecordMeshStreamV1::from(&cursor),
            after,
        });
    }
    let mut changed = false;
    let authorizer = DirectMeshAuthorizer {
        peer: &session.peer,
        identity,
        policy,
    };
    for chunk in pulls.chunks(MAX_PULL_STREAMS) {
        // Where each requested stream ends here; every batch must continue
        // exactly there.
        let mut next_after: BTreeMap<crate::record_repository::RecordStreamKey, u64> = chunk
            .iter()
            .map(|p| {
                (
                    record_reconcile::stream_key(&p.stream),
                    p.after.as_ref().map_or(0, |c| c.contiguous_sequence),
                )
            })
            .collect();
        let (mut send, mut recv) = session
            .open(&pool.request(SessionOperationV1::Pull {
                streams: chunk.to_vec(),
                inline_content_bytes: INLINE_CONTENT_BYTES,
            }))
            .await?;
        let _ = send.finish();
        let mut touched = Vec::new();
        let mut inline = Vec::new();
        while let Some(frame) = session.frames(&mut recv, MAX_FRAME_BYTES).await? {
            match frame {
                SessionFrameV1::Envelopes { batches } => {
                    for batch in batches {
                        // Each batch must continue exactly where this device's
                        // copy of the stream ends.
                        let Some(after) =
                            next_after.get_mut(&record_reconcile::stream_key(&batch.stream))
                        else {
                            anyhow::bail!("pull returned a stream that was not requested");
                        };
                        anyhow::ensure!(
                            batch.after_sequence == *after
                                && !batch.envelopes.is_empty()
                                && batch.envelopes.len() <= MAX_RECORD_BATCH_OPERATIONS,
                            "pull returned a non-contiguous batch"
                        );
                        let key = RecordRepositoryKey {
                            workspace_id: records.workspace_id,
                            record_id: batch.stream.record_id,
                        };
                        let mut expected = batch.after_sequence;
                        for envelope in &batch.envelopes {
                            expected = expected.saturating_add(1);
                            anyhow::ensure!(
                                envelope.record_id == batch.stream.record_id
                                    && envelope.actor == batch.stream.actor
                                    && envelope.sequence == expected,
                                "pull returned an envelope outside its stream"
                            );
                        }
                        records
                            .repository
                            .apply_batch(key, &batch.envelopes, &authorizer)?;
                        *after = expected;
                        changed = true;
                        touched.push(key);
                    }
                }
                SessionFrameV1::StreamFailed { stream, code } => {
                    eprintln!(
                        "record stream {:?} from {} failed: {code:?}",
                        stream.record_id, session.addr.id
                    );
                }
                SessionFrameV1::Blobs { blobs } => {
                    inline.extend(
                        blobs
                            .into_iter()
                            .map(|b| (iroh_blobs::Hash::from(b.hash), Bytes::from(b.bytes))),
                    );
                }
                _ => anyhow::bail!("unexpected pull frame"),
            }
        }
        // Content first, so a record that becomes visible can be read.
        if !inline.is_empty()
            && let Err(error) = hydrator.accept_inline(&session.addr, inline).await
        {
            eprintln!(
                "inline content from {} rejected: {error:#}",
                session.addr.id
            );
        }
        if let Some(materializer) = &records.materializer {
            touched.sort();
            touched.dedup();
            for key in touched {
                materializer.reconcile_record(&records.repository, key)?;
            }
        }
    }
    // Records that arrived before their namespace placement are projected
    // once the placement exists: after namespace changes, and whenever the
    // caller asks (the first pass after startup).
    if (changed || heal)
        && let Some(materializer) = &records.materializer
    {
        materializer.reconcile_repository(&records.repository)?;
    }
    Ok(changed)
}

/// Fresh receipts from the peer for every current revision it has not
/// acknowledged recently, many per request, committed to the availability
/// registry in one write.
pub(super) async fn collect_replica_acks(
    pool: &SessionPool,
    session: &Session,
    identity: &FsTransportIdentity,
    records: &RecordPlane,
) -> Result<()> {
    let Some(registry) = &records.availability else {
        return Ok(());
    };
    let device = collab_types::Ed25519PublicKey(session.peer.device_public_key);
    let requests: Vec<ReplicaAckRequestV1> =
        super::revisions_awaiting_receipt(records, registry, device)
            .into_iter()
            .filter_map(|revision| ReplicaAckRequestV1::new(records.workspace_id, revision).ok())
            .collect();
    for chunk in requests.chunks(MAX_REPLICA_ACKS) {
        let (mut send, mut recv) = session
            .open(&pool.request(SessionOperationV1::ReplicaAcks {
                requests: chunk.to_vec(),
            }))
            .await?;
        let _ = send.finish();
        let mut responses = Vec::new();
        while let Some(frame) = session.frames(&mut recv, MAX_FRAME_BYTES).await? {
            if let SessionFrameV1::ReplicaAcks { responses: part } = frame {
                responses.extend(part);
            }
        }
        let local = collab_types::Ed25519PublicKey(identity.device_public_key());
        let observed_at = unix_now_ms();
        let mut observations = Vec::new();
        for (request, response) in chunk.iter().zip(responses) {
            let ReplicaAckResponseV1::Acknowledged(receipt) = response else {
                continue;
            };
            if let Ok(observation) = receipt.verify_for(request, &session.peer, local, observed_at)
            {
                observations.push((request.revision, observation));
            }
        }
        if observations.is_empty() {
            continue;
        }
        let mut registry = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.batch(|registry| {
            for (revision, observation) in observations {
                let observed = observation.observed_at_ms;
                let _ = super::record_replica_observation(
                    registry,
                    revision,
                    local,
                    observation,
                    observed,
                );
            }
        })?;
    }
    Ok(())
}
