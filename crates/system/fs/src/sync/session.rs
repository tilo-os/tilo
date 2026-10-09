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
use wire_frame::{FrameError, Framing};

use super::{
    DirectMeshAuthorizer, FsTransportIdentity, PeerPolicy, RecordPlane, accept_namespace_payload,
    authenticate_connection, authenticated_peer_may_read_whole_workspace,
    authenticated_peer_may_write_whole_workspace, unix_now_ms, unix_now_ns,
};
use crate::hydration::Hydrator;
use crate::namespace::Namespace;
use crate::namespace_binding::{BoundNamespacePayloadV1, NamespaceBindingV1, NamespacePayloadKind};
use crate::peer_auth::AuthenticatedFsPeer;
use crate::record_changeset::{ChangesetLeafV1, ChangesetV1, SignedChangesetV1};
use crate::record_materializer::ReconcileScope;
use crate::record_mesh::{RecordMeshErrorCodeV1, RecordMeshStreamV1};
use crate::record_reconcile::{self, ReconcileAnswerV1, ReconcileRangeV1};
use crate::record_replication::{RecordEnvelopeV1, RecordOperationV1};
use crate::record_repository::{
    ChangesetApplied, MAX_RECORD_BATCH_OPERATIONS, PullUnit, RecordRepositoryKey,
    RecordStreamCursor,
};
use crate::replica_ack::{ReplicaAckRequestV1, ReplicaAckResponseV1};
use crate::share_session::LiveActorCertification;
use crate::store::ContentStore;
use crate::workspace_authority::{AuthorityRequest, AuthorityResponse, WorkspaceAuthority};

pub const TILO_FS_SESSION_ALPN: &[u8] = b"tilo-fs-session/2";
pub const SESSION_PROTOCOL_VERSION: u16 = 2;

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
/// Changeset leaves per frame (a leaf is about 70 bytes on the wire).
const CHANGESET_LEAVES_PER_FRAME: usize = 8 * 1024;
/// Leaves one changeset may have: the repository's record bound.
const MAX_CHANGESET_LEAVES: u64 = crate::record_repository::MAX_REPOSITORY_RECORDS as u64;
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
    /// The namespace changes beyond `heads`, for a read-only peer: as
    /// `NamespaceChunk` frames, then the binding of all of them.
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
    /// As `Pull`, except that envelopes belonging to a signed changeset
    /// come as that whole changeset (`ChangesetBegin`, its leaves and
    /// envelopes, `ChangesetEnd`), which the requester applies atomically.
    /// Units come in the order they entered the server's log. Peers that do
    /// not know this request fail it, and are pulled from with `Pull`.
    PullChangesets {
        streams: Vec<SessionPullV1>,
        inline_content_bytes: u32,
    },
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
    /// One sync round's message, bound to the workspace by its sender;
    /// `None` when that side has nothing to send.
    Sync(Option<BoundNamespacePayloadV1>),
    NamespaceChunk(#[serde(with = "serde_bytes")] Vec<u8>),
    /// The binding of the concatenated `NamespaceChunk` bytes before it.
    NamespaceBound(NamespaceBindingV1),
    ReplicaAcks {
        responses: Vec<ReplicaAckResponseV1>,
    },
    Authority(AuthorityResponse),
    /// The response is complete.
    Done,
    Denied,
    Error(RecordMeshErrorCodeV1),
    /// A changeset follows: its signed header, then `ChangesetLeaves` and
    /// `Envelopes` frames up to `ChangesetEnd`.
    ChangesetBegin(ChangesetV1),
    ChangesetLeaves {
        leaves: Vec<ChangesetLeafV1>,
    },
    ChangesetEnd,
}

/// Every request and response frame: a big-endian `u32` length, then the
/// postcard encoding. Each read bounds the frame by what that step expects
/// (see [`read_frame`]); nothing larger than the largest of those bounds is
/// sent.
const SESSION_FRAMING: Framing = Framing::big_endian(MAX_SYNC_FRAME_BYTES);

async fn write_frame<T: Serialize + Sync>(send: &mut SendStream, value: &T) -> Result<()> {
    SESSION_FRAMING.write_value_async(send, value).await?;
    Ok(())
}

/// The next frame, of at most `limit` bytes; the stream closing before it
/// is an error.
async fn read_frame<T: serde::de::DeserializeOwned>(
    recv: &mut RecvStream,
    limit: usize,
) -> Result<T> {
    Ok(SESSION_FRAMING
        .with_max_len(limit)
        .read_value_async(recv)
        .await?
        .ok_or(FrameError::Closed)?)
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
    /// This device's sessions: a peer's incoming session is also this
    /// device's session to that peer.
    pub pool: std::sync::Weak<SessionPool>,
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
        if let Some(pool) = self.pool.upgrade() {
            pool.adopt(&connection, &peer).await;
        }
        self.serve_connection(&connection, &peer).await;
        Ok(())
    }
}

impl SessionProtocol {
    /// Serves the requests `peer` opens on `connection` until it closes.
    /// Both ends of a session serve: the device that opened it and the one
    /// that accepted it each send requests on it.
    async fn serve_connection(&self, connection: &Connection, peer: &AuthenticatedFsPeer) {
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
    }

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
            SessionOperationV1::PullChangesets {
                streams,
                inline_content_bytes,
            } => {
                self.serve_pull_changesets(peer, &streams, inline_content_bytes, &mut send)
                    .await?;
            }
        }
        send.finish()?;
        Ok(())
    }

    /// Waits out a changeset being published here: namespace state read
    /// under this guard holds each changeset entirely or not at all.
    async fn publication_gate(&self) -> Option<tokio::sync::RwLockReadGuard<'_, ()>> {
        match &self.records {
            Some(records) => Some(records.repository.publication_gate().read().await),
            None => None,
        }
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
        self.send_inline(current, inline_limit, send).await?;
        write_frame(send, &SessionFrameV1::Done).await
    }

    /// The bytes of the small files `current` names, in frames.
    async fn send_inline(
        &self,
        current: BTreeMap<collab_types::FileRecordId, [u8; 32]>,
        inline_limit: u64,
        send: &mut SendStream,
    ) -> Result<()> {
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
        Ok(())
    }

    /// `PullChangesets`: what the requester misses on each stream, as whole
    /// changesets where the envelopes belong to one, in log order.
    #[allow(
        clippy::too_many_lines,
        reason = "one response validates the cursors, then frames each unit in log order"
    )]
    async fn serve_pull_changesets(
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
        let mut wanted = Vec::with_capacity(streams.len());
        for pull in streams {
            let stream = pull.stream;
            let key = RecordRepositoryKey {
                workspace_id: records.workspace_id,
                record_id: stream.record_id,
            };
            let failed = |code| SessionFrameV1::StreamFailed { stream, code };
            if let Some(cursor) = &pull.after {
                if cursor.record_id != stream.record_id || cursor.actor != stream.actor {
                    write_frame(send, &failed(RecordMeshErrorCodeV1::InvalidRequest)).await?;
                    continue;
                }
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
            wanted.push((
                stream.record_id,
                stream.actor,
                pull.after.as_ref().map_or(0, |c| c.contiguous_sequence),
            ));
        }
        let Ok(units) = records.repository.pull_units(records.workspace_id, &wanted) else {
            return write_frame(
                send,
                &SessionFrameV1::Error(RecordMeshErrorCodeV1::Internal),
            )
            .await;
        };
        // The content each sent record ends at, per record.
        let mut current: BTreeMap<collab_types::FileRecordId, [u8; 32]> = BTreeMap::new();
        let mut note = |envelope: &RecordEnvelopeV1| {
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
            envelope
                .canonical_bytes()
                .map_or(TARGET_FRAME_BYTES, |bytes| bytes.len())
        };
        let mut batches = Vec::new();
        let mut size = 0_usize;
        for unit in units {
            match unit {
                PullUnit::Stream {
                    key,
                    actor,
                    after_sequence,
                    envelopes,
                } => {
                    for envelope in &envelopes {
                        size = size.saturating_add(note(envelope));
                    }
                    batches.push(SessionEnvelopesV1 {
                        stream: RecordMeshStreamV1 {
                            record_id: key.record_id,
                            actor,
                        },
                        after_sequence,
                        envelopes,
                    });
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
                }
                PullUnit::Changeset(changeset) => {
                    if !batches.is_empty() {
                        write_frame(
                            send,
                            &SessionFrameV1::Envelopes {
                                batches: std::mem::take(&mut batches),
                            },
                        )
                        .await?;
                        size = 0;
                    }
                    write_frame(
                        send,
                        &SessionFrameV1::ChangesetBegin(changeset.header.clone()),
                    )
                    .await?;
                    for leaves in changeset.leaves.chunks(CHANGESET_LEAVES_PER_FRAME) {
                        write_frame(
                            send,
                            &SessionFrameV1::ChangesetLeaves {
                                leaves: leaves.to_vec(),
                            },
                        )
                        .await?;
                    }
                    for leaf in &changeset.leaves {
                        let Ok(envelopes) = records
                            .repository
                            .changeset_leaf_envelopes(&changeset, leaf)
                        else {
                            return write_frame(
                                send,
                                &SessionFrameV1::Error(RecordMeshErrorCodeV1::Internal),
                            )
                            .await;
                        };
                        for chunk in envelopes.chunks(MAX_RECORD_BATCH_OPERATIONS) {
                            for envelope in chunk {
                                size = size.saturating_add(note(envelope));
                            }
                            batches.push(SessionEnvelopesV1 {
                                stream: RecordMeshStreamV1 {
                                    record_id: leaf.record_id,
                                    actor: changeset.header.actor,
                                },
                                after_sequence: chunk
                                    .first()
                                    .map_or(0, |e| e.sequence.saturating_sub(1)),
                                envelopes: chunk.to_vec(),
                            });
                        }
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
                    }
                    if !batches.is_empty() {
                        write_frame(
                            send,
                            &SessionFrameV1::Envelopes {
                                batches: std::mem::take(&mut batches),
                            },
                        )
                        .await?;
                        size = 0;
                    }
                    write_frame(send, &SessionFrameV1::ChangesetEnd).await?;
                }
            }
        }
        if !batches.is_empty() {
            write_frame(send, &SessionFrameV1::Envelopes { batches }).await?;
        }
        // Inline content: the requester may read every record it was just
        // sent (whole-workspace read), so it may read their current bytes.
        self.send_inline(current, inline_limit, send).await?;
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
        let total = decoded.total_len();
        if total > limit {
            return Vec::new();
        }
        let mut out = vec![SessionBlobV1 {
            hash: *manifest.as_bytes(),
            bytes: bytes.to_vec(),
        }];
        for chunk in decoded.chunks() {
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
        let (before, mut state) = {
            let mut namespace = self
                .namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                namespace.heads(),
                namespace.begin_sync_with(&peer.device_public_key),
            )
        };
        let exchanged = self.namespace_rounds(peer, &mut state, send, recv).await;
        // Only a completed exchange is resumed from: one cut off midway
        // may not have agreed on what both sides hold.
        if matches!(exchanged, Ok(true)) {
            self.namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .finish_sync_with(peer.device_public_key, &state);
        }
        // Records that arrived before the placements this exchange brought
        // are projected now, not at this device's next pass.
        let changed = self
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .heads()
            != before;
        if changed
            && let Some(records) = &self.records
            && let Some(materializer) = &records.materializer
        {
            let gate = records.repository.publication_gate().write().await;
            if let Err(error) =
                materializer.reconcile_batch(&records.repository, &[], ReconcileScope::Unplaced)
            {
                eprintln!("projecting records after a namespace exchange failed: {error}");
            }
            drop(gate);
        }
        exchanged.map(|_| ())
    }

    /// Whether the exchange completed: both sides had nothing more to send.
    async fn namespace_rounds(
        &self,
        peer: &AuthenticatedFsPeer,
        state: &mut automerge::sync::State,
        send: &mut SendStream,
        recv: &mut RecvStream,
    ) -> Result<bool> {
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
            let message = match message {
                Some(payload) => {
                    if let Err(error) = accept_namespace_payload(
                        &self.policy,
                        self.workspace_id,
                        peer,
                        NamespacePayloadKind::SyncMessage,
                        &payload,
                    ) {
                        eprintln!("refusing a namespace sync message over a session: {error}");
                        break;
                    }
                    payload.bytes
                }
                None => Vec::new(),
            };
            if !message.is_empty() {
                let _ = self
                    .namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .sync_receive(state, &message);
            }
            let gate = self.publication_gate().await;
            let reply = self
                .namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sync_generate(state);
            drop(gate);
            let done = message.is_empty() && reply.is_none();
            let reply = reply
                .map(|reply| {
                    self.identity.bind_namespace(
                        self.workspace_id,
                        NamespacePayloadKind::SyncMessage,
                        reply,
                    )
                })
                .transpose()?;
            write_frame(send, &SessionFrameV1::Sync(reply)).await?;
            if done {
                return Ok(true);
            }
        }
        Ok(false)
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
        let gate = self.publication_gate().await;
        let delta = self
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .changes_since(&heads);
        drop(gate);
        if !delta.is_empty() {
            let binding = NamespaceBindingV1::sign(
                self.workspace_id,
                NamespacePayloadKind::Changes,
                &delta,
                self.identity.credentials.signing_key(),
            )?;
            for chunk in delta.chunks(TARGET_FRAME_BYTES) {
                write_frame(send, &SessionFrameV1::NamespaceChunk(chunk.to_vec())).await?;
            }
            write_frame(send, &SessionFrameV1::NamespaceBound(binding)).await?;
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
    /// Whether the peer answers `PullChangesets` (until it fails one).
    changesets: std::sync::atomic::AtomicBool,
}

/// Sessions to every peer this device reconciles with, opened on first use.
pub(super) struct SessionPool {
    endpoint: Endpoint,
    identity: FsTransportIdentity,
    workspace_id: uuid::Uuid,
    sessions: tokio::sync::Mutex<HashMap<iroh_base::PublicKey, Arc<Session>>>,
    legacy: Mutex<HashMap<iroh_base::PublicKey, Instant>>,
    /// Serves the requests peers send on sessions this device opened.
    server: std::sync::OnceLock<SessionProtocol>,
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
            server: std::sync::OnceLock::new(),
        }
    }

    /// Serves peers' requests on the sessions this pool opens, including
    /// those it opened before (a joining device's first pull). Set once.
    pub(super) fn serve_with(self: &Arc<Self>, protocol: SessionProtocol) {
        if self.server.set(protocol.clone()).is_err() {
            return;
        }
        let pool = Arc::clone(self);
        tokio::spawn(async move {
            let open: Vec<Arc<Session>> = pool
                .sessions
                .lock()
                .await
                .values()
                .filter(|session| !session.is_closed())
                .cloned()
                .collect();
            for session in open {
                let protocol = protocol.clone();
                tokio::spawn(async move {
                    protocol
                        .serve_connection(&session.connection, &session.peer)
                        .await;
                });
            }
        });
    }

    /// Takes a peer's incoming, authenticated session as this device's
    /// session to that peer too. The device that could reach the other is
    /// then reachable back on it, whether or not the other could dial it:
    /// behind NAT, or restarted on a new address. It replaces an earlier
    /// session, which may belong to the peer's previous run and look open
    /// until it times out; that one still serves whatever the peer sends on
    /// it.
    pub(super) async fn adopt(&self, connection: &Connection, peer: &AuthenticatedFsPeer) {
        let id = connection.remote_id();
        self.sessions.lock().await.insert(
            id,
            Arc::new(Session {
                connection: connection.clone(),
                peer: peer.clone(),
                addr: EndpointAddr::new(id),
                last_heard: Mutex::new(Instant::now()),
                changesets: std::sync::atomic::AtomicBool::new(true),
            }),
        );
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
                    changesets: std::sync::atomic::AtomicBool::new(true),
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
                        if let Some(server) = self.server.get() {
                            let server = server.clone();
                            let session = Arc::clone(&session);
                            tokio::spawn(async move {
                                server
                                    .serve_connection(&session.connection, &session.peer)
                                    .await;
                            });
                        }
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
/// changes were received. Each message the peer sends is applied only
/// once [`accept_namespace_payload`] accepts it for this workspace.
pub(super) async fn sync_namespace(
    pool: &SessionPool,
    session: &Session,
    namespace: &Arc<Mutex<Namespace>>,
    repository: Option<&crate::record_repository::RecordRepository>,
    policy: &PeerPolicy,
) -> Result<bool> {
    let (mut send, mut recv) = session
        .open(&pool.request(SessionOperationV1::NamespaceSync))
        .await?;
    if session.frames(&mut recv, MAX_FRAME_BYTES).await?.is_some() {
        anyhow::bail!("unexpected namespace sync response");
    }
    let peer = session.peer.device_public_key;
    let mut state = namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .begin_sync_with(&peer);
    let mut received = false;
    let mut completed = false;
    for _ in 0..MAX_SYNC_ROUNDS {
        // Only namespace state between changesets leaves this device.
        let gate = match repository {
            Some(repository) => Some(repository.publication_gate().read().await),
            None => None,
        };
        let message = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sync_generate(&mut state);
        drop(gate);
        let sent_nothing = message.is_none();
        let message = message
            .map(|message| {
                pool.identity.bind_namespace(
                    pool.workspace_id,
                    NamespacePayloadKind::SyncMessage,
                    message,
                )
            })
            .transpose()?;
        write_frame(&mut send, &SessionFrameV1::Sync(message)).await?;
        let SessionFrameV1::Sync(reply) = read_frame(&mut recv, MAX_SYNC_FRAME_BYTES).await? else {
            anyhow::bail!("unexpected namespace sync frame");
        };
        session.heard();
        let reply = match reply {
            Some(payload) => {
                accept_namespace_payload(
                    policy,
                    pool.workspace_id,
                    &session.peer,
                    NamespacePayloadKind::SyncMessage,
                    &payload,
                )
                .map_err(|error| anyhow!("refusing the peer's namespace sync message: {error}"))?;
                payload.bytes
            }
            None => Vec::new(),
        };
        if !reply.is_empty() {
            received = true;
            let _ = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .sync_receive(&mut state, &reply);
        }
        if sent_nothing && reply.is_empty() {
            completed = true;
            break;
        }
    }
    let _ = send.finish();
    if completed {
        namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish_sync_with(peer, &state);
    }
    Ok(received)
}

/// The namespace changes a peer holds beyond this device's heads, for a
/// read-only device. Returns the resolution delta and whether anything was
/// applied. The changes are applied only once [`accept_namespace_payload`]
/// accepts them for this workspace.
pub(super) async fn pull_namespace(
    pool: &SessionPool,
    session: &Session,
    namespace: &Arc<Mutex<Namespace>>,
    policy: &PeerPolicy,
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
    let mut binding = None;
    while let Some(frame) = session.frames(&mut recv, MAX_FRAME_BYTES).await? {
        match frame {
            SessionFrameV1::NamespaceChunk(chunk) if binding.is_none() => {
                anyhow::ensure!(
                    delta.len().saturating_add(chunk.len()) <= MAX_NAMESPACE_BYTES,
                    "namespace delta exceeds {MAX_NAMESPACE_BYTES} bytes"
                );
                delta.extend_from_slice(&chunk);
            }
            SessionFrameV1::NamespaceBound(bound) if binding.is_none() => binding = Some(bound),
            _ => anyhow::bail!("unexpected namespace frame"),
        }
    }
    if delta.is_empty() {
        return Ok((false, Vec::new()));
    }
    let payload = BoundNamespacePayloadV1 {
        binding: binding.ok_or_else(|| {
            anyhow!(
                "refusing the peer's namespace changes: {}",
                crate::namespace_binding::NamespaceBindingError::Unbound
            )
        })?,
        bytes: delta,
    };
    accept_namespace_payload(
        policy,
        pool.workspace_id,
        &session.peer,
        NamespacePayloadKind::Changes,
        &payload,
    )
    .map_err(|error| anyhow!("refusing the peer's namespace changes: {error}"))?;
    let delta = payload.bytes;
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

/// One requested stream: what to ask for, and how far the peer is.
struct WantedStream {
    stream: RecordMeshStreamV1,
    remote_sequence: u64,
}

/// What one pull request brought.
#[derive(Default)]
struct Pulled {
    changed: bool,
    touched: Vec<RecordRepositoryKey>,
    inline: Vec<(iroh_blobs::Hash, Bytes)>,
    /// Time spent verifying and writing what arrived.
    applying: Duration,
}

/// The peer does not answer `PullChangesets`.
#[derive(Debug)]
struct ChangesetsUnsupported;

impl std::fmt::Display for ChangesetsUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("peer does not serve changesets")
    }
}

impl std::error::Error for ChangesetsUnsupported {}

/// The requests for `wanted` streams this device is still behind on, from
/// where each stream ends here now.
fn behind(records: &RecordPlane, wanted: &[WantedStream]) -> Result<Vec<SessionPullV1>> {
    let mut pulls = Vec::new();
    for want in wanted {
        let key = RecordRepositoryKey {
            workspace_id: records.workspace_id,
            record_id: want.stream.record_id,
        };
        let local = records
            .repository
            .highest_sequence(key, &want.stream.actor)
            .unwrap_or(0);
        if local >= want.remote_sequence {
            continue;
        }
        let after = if local == 0 {
            None
        } else {
            Some(
                records
                    .repository
                    .stream_cursor(key, &want.stream.actor, local)?,
            )
        };
        pulls.push(SessionPullV1 {
            stream: want.stream,
            after,
        });
    }
    Ok(pulls)
}

/// The peer's streams (of its own person) that are ahead of this device.
fn streams_ahead(
    session: &Session,
    records: &RecordPlane,
    remote: Vec<RecordStreamCursor>,
) -> Vec<WantedStream> {
    let mut wanted = Vec::new();
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
        if local == cursor.contiguous_sequence
            && local > 0
            && records
                .repository
                .stream_cursor(key, &cursor.actor, local)
                .is_ok_and(|after| after != cursor)
        {
            eprintln!(
                "record stream {:?} diverged from {}",
                cursor.record_id, session.addr.id
            );
        }
        if local >= cursor.contiguous_sequence {
            continue;
        }
        wanted.push(WantedStream {
            stream: RecordMeshStreamV1::from(&cursor),
            remote_sequence: cursor.contiguous_sequence,
        });
    }
    wanted
}

/// Pulls every record stream the peer is ahead on: reconciliation, then
/// batched pulls of many streams each, with small files' content inline.
/// Envelopes of a signed changeset arrive and are applied as that whole
/// changeset, and every record the pulls changed is projected in one step
/// under the publication gate, so the namespace never holds half of a
/// changeset. Returns whether anything was applied. `scope` says which
/// records besides the pulled ones are projected again.
pub(super) async fn pull_records(
    pool: &SessionPool,
    session: &Session,
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    records: &RecordPlane,
    hydrator: &Hydrator,
    scope: ReconcileScope,
) -> Result<bool> {
    let remote = reconcile_streams(pool, session, records).await?;
    let wanted = streams_ahead(session, records, remote);
    let authorizer = DirectMeshAuthorizer {
        peer: &session.peer,
        identity,
        policy,
    };
    let mut pulled = Pulled::default();
    let exchange = Instant::now();
    for chunk in wanted.chunks(MAX_PULL_STREAMS) {
        // A changeset an earlier request brought may already cover these.
        let pulls = behind(records, chunk)?;
        if pulls.is_empty() {
            continue;
        }
        let part = if session
            .changesets
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            match pull_changesets(pool, session, records, &authorizer, pulls.clone()).await {
                Err(error) if error.is::<ChangesetsUnsupported>() => {
                    session
                        .changesets
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    pull_streams(pool, session, records, &authorizer, pulls).await?
                }
                other => other?,
            }
        } else {
            pull_streams(pool, session, records, &authorizer, pulls).await?
        };
        pulled.changed |= part.changed;
        pulled.touched.extend(part.touched);
        pulled.inline.extend(part.inline);
        pulled.applying = pulled.applying.saturating_add(part.applying);
    }
    let exchanged = exchange.elapsed();
    let inline = pulled.inline.len();
    // Content first, so a record that becomes visible can be read.
    if !pulled.inline.is_empty()
        && let Err(error) = hydrator
            .accept_inline(&session.addr, std::mem::take(&mut pulled.inline))
            .await
    {
        eprintln!(
            "inline content from {} rejected: {error:#}",
            session.addr.id
        );
    }
    if !pulled.touched.is_empty() && std::env::var_os("TILO_FS_NATIVE_TRACE").is_some() {
        eprintln!(
            "  pulled {} record(s) in {} ms ({} ms applying them), {} inline blob(s) stored in {} ms",
            pulled.touched.len(),
            exchanged.as_millis(),
            pulled.applying.as_millis(),
            inline,
            exchange.elapsed().saturating_sub(exchanged).as_millis()
        );
    }
    // Records that arrived before their namespace placement are projected
    // once the placement exists: `scope` says what else to project.
    if let Some(materializer) = &records.materializer
        && (scope != ReconcileScope::Touched || !pulled.touched.is_empty())
    {
        pulled.touched.sort();
        pulled.touched.dedup();
        let gate = records.repository.publication_gate().write().await;
        let started = Instant::now();
        let projected = materializer.reconcile_batch(&records.repository, &pulled.touched, scope);
        drop(gate);
        if !pulled.touched.is_empty() && std::env::var_os("TILO_FS_NATIVE_TRACE").is_some() {
            eprintln!(
                "  {} pulled record(s) projected into the namespace in {} ms",
                pulled.touched.len(),
                started.elapsed().as_millis()
            );
        }
        projected?;
    }
    Ok(pulled.changed)
}

/// One `PullChangesets` request: changesets are applied whole as each
/// completes, other envelopes per frame.
#[allow(
    clippy::too_many_lines,
    reason = "one ordered response stream of changesets and stream batches"
)]
async fn pull_changesets(
    pool: &SessionPool,
    session: &Session,
    records: &RecordPlane,
    authorizer: &DirectMeshAuthorizer<'_>,
    pulls: Vec<SessionPullV1>,
) -> Result<Pulled> {
    let (mut send, mut recv) = session
        .open(&pool.request(SessionOperationV1::PullChangesets {
            streams: pulls,
            inline_content_bytes: INLINE_CONTENT_BYTES,
        }))
        .await?;
    let _ = send.finish();
    let mut pulled = Pulled::default();
    let mut first = true;
    // The changeset being received: header, leaves, envelopes, and how
    // many envelopes its leaves name.
    let mut open: Option<(
        ChangesetV1,
        Vec<ChangesetLeafV1>,
        Vec<RecordEnvelopeV1>,
        u64,
    )> = None;
    loop {
        let frame = match session.frames(&mut recv, MAX_FRAME_BYTES).await {
            Ok(frame) => frame,
            // A peer that does not know the request drops it unanswered.
            Err(error) if first && !format!("{error:#}").contains("denied") => {
                let _ = error;
                return Err(ChangesetsUnsupported.into());
            }
            Err(error) => return Err(error),
        };
        first = false;
        let Some(frame) = frame else {
            break;
        };
        match frame {
            SessionFrameV1::ChangesetBegin(header) => {
                anyhow::ensure!(open.is_none(), "changeset began inside another");
                anyhow::ensure!(
                    header.workspace_id == records.workspace_id
                        && header.leaf_count <= MAX_CHANGESET_LEAVES,
                    "changeset for another workspace or too large"
                );
                open = Some((header, Vec::new(), Vec::new(), 0));
            }
            SessionFrameV1::ChangesetLeaves { leaves } => {
                let Some((header, all, _, named)) = open.as_mut() else {
                    anyhow::bail!("changeset leaves outside a changeset");
                };
                anyhow::ensure!(
                    (all.len() as u64).saturating_add(leaves.len() as u64) <= header.leaf_count,
                    "more changeset leaves than signed"
                );
                for leaf in &leaves {
                    *named = named.saturating_add(
                        leaf.last_sequence
                            .saturating_sub(leaf.first_sequence)
                            .saturating_add(1),
                    );
                }
                all.extend(leaves);
            }
            SessionFrameV1::Envelopes { batches } => {
                if let Some((_, _, envelopes, named)) = open.as_mut() {
                    for batch in batches {
                        anyhow::ensure!(
                            (envelopes.len() as u64).saturating_add(batch.envelopes.len() as u64)
                                <= *named,
                            "more changeset envelopes than its leaves name"
                        );
                        envelopes.extend(batch.envelopes);
                    }
                } else {
                    let part = apply_stream_batches(records, authorizer, batches)?;
                    pulled.changed |= !part.is_empty();
                    pulled.touched.extend(part);
                }
            }
            SessionFrameV1::ChangesetEnd => {
                let Some((header, leaves, envelopes, _)) = open.take() else {
                    anyhow::bail!("changeset ended without beginning");
                };
                let changeset = SignedChangesetV1 { header, leaves };
                let applying = Instant::now();
                let applied = records.repository.apply_changeset(
                    records.workspace_id,
                    &changeset,
                    &envelopes,
                    authorizer,
                )?;
                pulled.applying = pulled.applying.saturating_add(applying.elapsed());
                match applied {
                    ChangesetApplied::Applied(keys) => {
                        if std::env::var_os("TILO_FS_NATIVE_TRACE").is_some() {
                            eprintln!(
                                "  changeset {} of {} record(s) applied from {}",
                                changeset.header.sequence,
                                changeset.header.leaf_count,
                                session.addr.id.fmt_short()
                            );
                        }
                        pulled.changed = true;
                        pulled.touched.extend(keys);
                    }
                    ChangesetApplied::AlreadyPresent => {}
                }
            }
            SessionFrameV1::StreamFailed { stream, code } => {
                eprintln!(
                    "record stream {:?} from {} failed: {code:?}",
                    stream.record_id, session.addr.id
                );
            }
            SessionFrameV1::Blobs { blobs } => {
                pulled.inline.extend(
                    blobs
                        .into_iter()
                        .map(|b| (iroh_blobs::Hash::from(b.hash), Bytes::from(b.bytes))),
                );
            }
            _ => anyhow::bail!("unexpected pull frame"),
        }
    }
    anyhow::ensure!(open.is_none(), "a changeset was cut off");
    Ok(pulled)
}

/// Applies stream batches outside any changeset in one durable write. A
/// batch may repeat envelopes this device already holds; those are dropped,
/// and what remains must continue exactly where the stream ends here.
fn apply_stream_batches(
    records: &RecordPlane,
    authorizer: &DirectMeshAuthorizer<'_>,
    batches: Vec<SessionEnvelopesV1>,
) -> Result<Vec<RecordRepositoryKey>> {
    let mut staged: Vec<(RecordRepositoryKey, Vec<RecordEnvelopeV1>)> = Vec::new();
    let mut ends: BTreeMap<crate::record_repository::RecordStreamKey, u64> = BTreeMap::new();
    for batch in batches {
        anyhow::ensure!(
            !batch.envelopes.is_empty() && batch.envelopes.len() <= MAX_RECORD_BATCH_OPERATIONS,
            "pull returned an empty or oversized batch"
        );
        let key = RecordRepositoryKey {
            workspace_id: records.workspace_id,
            record_id: batch.stream.record_id,
        };
        let stream_key = record_reconcile::stream_key(&batch.stream);
        let end = match ends.get(&stream_key) {
            Some(end) => *end,
            None => records
                .repository
                .highest_sequence(key, &batch.stream.actor)
                .unwrap_or(0),
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
        let fresh: Vec<RecordEnvelopeV1> = batch
            .envelopes
            .into_iter()
            .filter(|envelope| envelope.sequence > end)
            .collect();
        let Some(last) = fresh.last().map(|e| e.sequence) else {
            continue;
        };
        anyhow::ensure!(
            fresh.first().map(|e| e.sequence) == Some(end.saturating_add(1)),
            "pull returned a non-contiguous batch"
        );
        ends.insert(stream_key, last);
        staged.push((key, fresh));
    }
    records.repository.apply_batches(&staged, authorizer)?;
    Ok(staged.into_iter().map(|(key, _)| key).collect())
}

/// One `Pull` request, for peers without changesets.
async fn pull_streams(
    pool: &SessionPool,
    session: &Session,
    records: &RecordPlane,
    authorizer: &DirectMeshAuthorizer<'_>,
    pulls: Vec<SessionPullV1>,
) -> Result<Pulled> {
    let (mut send, mut recv) = session
        .open(&pool.request(SessionOperationV1::Pull {
            streams: pulls,
            inline_content_bytes: INLINE_CONTENT_BYTES,
        }))
        .await?;
    let _ = send.finish();
    let mut pulled = Pulled::default();
    while let Some(frame) = session.frames(&mut recv, MAX_FRAME_BYTES).await? {
        match frame {
            SessionFrameV1::Envelopes { batches } => {
                let part = apply_stream_batches(records, authorizer, batches)?;
                pulled.changed |= !part.is_empty();
                pulled.touched.extend(part);
            }
            SessionFrameV1::StreamFailed { stream, code } => {
                eprintln!(
                    "record stream {:?} from {} failed: {code:?}",
                    stream.record_id, session.addr.id
                );
            }
            SessionFrameV1::Blobs { blobs } => {
                pulled.inline.extend(
                    blobs
                        .into_iter()
                        .map(|b| (iroh_blobs::Hash::from(b.hash), Bytes::from(b.bytes))),
                );
            }
            _ => anyhow::bail!("unexpected pull frame"),
        }
    }
    Ok(pulled)
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

#[cfg(test)]
mod tests;
