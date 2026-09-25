//! The peer half of document media: serving referenced blobs to room members'
//! devices, and fetching them from theirs.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler, RouterBuilder};
use iroh::{Endpoint, EndpointAddr};
use iroh_blobs::Hash;
use runtime_fs::blobs::{BlobFuture, BlobScope, MAX_BLOB_BYTES};
use serde::{Deserialize, Serialize};

use super::{MediaFetcher, references};
use crate::hydration::Hydrator;
use crate::peer_auth::AuthenticatedFsPeer;
use crate::store::ContentStore;
use crate::sync::{FsTransportIdentity, PeerPolicy};

/// Peer protocol: fetch one referenced blob of one room scope.
pub const TILO_FS_MEDIA_ALPN: &[u8] = b"tilo-fs-media/0";
const MEDIA_PROTOCOL_VERSION: u8 = 1;
const MAX_REQUEST_BYTES: usize = 1024;
const STATUS_READY: u8 = 0;
const STATUS_UNAVAILABLE: u8 = 1;
const PEER_FETCH_TIMEOUT: Duration = Duration::from_secs(20);
const SERVE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Serialize, Deserialize)]
struct MediaRequest {
    version: u8,
    room: String,
    app: String,
    hash: [u8; 32],
}

async fn authenticate(
    endpoint: &Endpoint,
    connection: &Connection,
    recv: &mut RecvStream,
    send: &mut SendStream,
    identity: &FsTransportIdentity,
) -> Result<AuthenticatedFsPeer> {
    let revocations = identity
        .revocations()
        .read()
        .map_err(|_| anyhow::anyhow!("peer revocation state is poisoned"))?
        .clone();
    Ok(
        crate::authenticated_transport::authenticate_fs_peer_parts_at(
            recv,
            send,
            &identity.credentials(),
            *endpoint.id().as_bytes(),
            *connection.remote_id().as_bytes(),
            &revocations,
            crate::clock::unix_now_ns(),
            crate::authenticated_transport::AuthenticatedTransportConfig::default(),
        )
        .await?,
    )
}

/// May `peer` receive media of `room` from this device? Its own person's
/// other devices may; anyone else needs room membership in the workspace
/// capability list. An unenforced workspace serves every authenticated peer.
fn peer_reads_room(
    policy: &PeerPolicy,
    identity: &FsTransportIdentity,
    peer: &AuthenticatedFsPeer,
    room: &str,
) -> bool {
    match policy {
        None => true,
        Some(acl) => {
            peer.user_public_key == identity.user_public_key()
                || acl.peer_is_room_member(&peer.user_public_key, room)
                || acl.peer_is_room_member(&peer.device_public_key, room)
        }
    }
}

/// Serves [`TILO_FS_MEDIA_ALPN`].
#[derive(Clone)]
pub(crate) struct MediaProtocol {
    endpoint: Endpoint,
    identity: FsTransportIdentity,
    policy: PeerPolicy,
    store: Arc<ContentStore>,
}

impl std::fmt::Debug for MediaProtocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MediaProtocol")
    }
}

impl MediaProtocol {
    async fn answer(&self, connection: &Connection) -> Result<()> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let peer = authenticate(
            &self.endpoint,
            connection,
            &mut recv,
            &mut send,
            &self.identity,
        )
        .await?;
        let request = read_request(&mut recv).await?;
        let content = match BlobScope::new(request.room, request.app) {
            Ok(scope) if peer_reads_room(&self.policy, &self.identity, &peer, scope.room()) => {
                let hash = Hash::from_bytes(request.hash);
                if references(&self.store, &scope, hash).await {
                    self.store.local(hash).await
                } else {
                    None
                }
            }
            _ => None,
        };
        match content {
            Some(bytes) => {
                send.write_all(&[STATUS_READY]).await?;
                send.write_all(&(bytes.len() as u64).to_le_bytes()).await?;
                send.write_all(&bytes).await?;
            }
            None => send.write_all(&[STATUS_UNAVAILABLE]).await?,
        }
        send.finish()?;
        Ok(())
    }
}

impl ProtocolHandler for MediaProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if self.identity.partitioned() {
            connection.close(1u32.into(), b"partitioned");
            return Ok(());
        }
        match tokio::time::timeout(SERVE_TIMEOUT, self.answer(&connection)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(AcceptError::from_err(std::io::Error::other(
                    error.to_string(),
                )));
            }
            Err(_) => {
                return Err(AcceptError::from_err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "document media request timed out",
                )));
            }
        }
        // Keep the connection until the requester has read the response.
        connection.closed().await;
        Ok(())
    }
}

/// Add the media protocol to a workspace router.
pub(crate) fn accept_media_protocol(
    builder: RouterBuilder,
    endpoint: &Endpoint,
    identity: &FsTransportIdentity,
    policy: &PeerPolicy,
    store: &Arc<ContentStore>,
) -> RouterBuilder {
    builder.accept(
        TILO_FS_MEDIA_ALPN,
        MediaProtocol {
            endpoint: endpoint.clone(),
            identity: identity.clone(),
            policy: policy.clone(),
            store: Arc::clone(store),
        },
    )
}

async fn read_request(recv: &mut RecvStream) -> Result<MediaRequest> {
    let mut length = [0_u8; 2];
    recv.read_exact(&mut length).await?;
    let length = usize::from(u16::from_le_bytes(length));
    anyhow::ensure!(
        (1..=MAX_REQUEST_BYTES).contains(&length),
        "media request length out of range"
    );
    let mut bytes = vec![0_u8; length];
    recv.read_exact(&mut bytes).await?;
    let request: MediaRequest = postcard::from_bytes(&bytes)?;
    anyhow::ensure!(
        request.version == MEDIA_PROTOCOL_VERSION,
        "unsupported media protocol version"
    );
    Ok(request)
}

/// Ask one peer for `hash` in `scope`; `Ok(None)` when it will not serve it.
///
/// # Errors
///
/// Returns an error when the peer cannot be reached, fails authentication,
/// or answers bytes that do not match `hash`.
pub async fn fetch_from_peer(
    endpoint: &Endpoint,
    identity: &FsTransportIdentity,
    peer: EndpointAddr,
    scope: &BlobScope,
    hash: Hash,
) -> Result<Option<Bytes>> {
    anyhow::ensure!(
        !identity.partitioned(),
        "transport partition gate is closed"
    );
    let connection = endpoint.connect(peer, TILO_FS_MEDIA_ALPN).await?;
    let (mut send, mut recv) = connection.open_bi().await?;
    authenticate(endpoint, &connection, &mut recv, &mut send, identity).await?;
    let request = postcard::to_stdvec(&MediaRequest {
        version: MEDIA_PROTOCOL_VERSION,
        room: scope.room().to_owned(),
        app: scope.app().to_owned(),
        hash: *hash.as_bytes(),
    })?;
    let length = u16::try_from(request.len())?;
    send.write_all(&length.to_le_bytes()).await?;
    send.write_all(&request).await?;
    send.finish()?;
    let mut status = [0_u8; 1];
    recv.read_exact(&mut status).await?;
    let result = if status[0] == STATUS_READY {
        let mut length = [0_u8; 8];
        recv.read_exact(&mut length).await?;
        let length = u64::from_le_bytes(length);
        anyhow::ensure!(length <= MAX_BLOB_BYTES, "peer offered oversized media");
        let mut bytes = vec![0_u8; usize::try_from(length)?];
        recv.read_exact(&mut bytes).await?;
        anyhow::ensure!(
            Hash::new(&bytes) == hash,
            "peer answered media that does not match its hash"
        );
        Some(Bytes::from(bytes))
    } else {
        None
    };
    connection.close(0u32.into(), b"");
    Ok(result)
}

/// Fetches from the workspace's known peers, one at a time.
pub struct PeerMediaFetcher {
    endpoint: Endpoint,
    identity: FsTransportIdentity,
    hydrator: Arc<Hydrator>,
}

impl PeerMediaFetcher {
    pub fn new(endpoint: Endpoint, identity: FsTransportIdentity, hydrator: Arc<Hydrator>) -> Self {
        Self {
            endpoint,
            identity,
            hydrator,
        }
    }
}

impl MediaFetcher for PeerMediaFetcher {
    fn fetch(&self, scope: BlobScope, hash: Hash) -> BlobFuture<Option<Bytes>> {
        let endpoint = self.endpoint.clone();
        let identity = self.identity.clone();
        let peers = self.hydrator.known_peers();
        Box::pin(async move {
            for peer in peers {
                let attempt = tokio::time::timeout(
                    PEER_FETCH_TIMEOUT,
                    fetch_from_peer(&endpoint, &identity, peer, &scope, hash),
                )
                .await;
                if let Ok(Ok(Some(bytes))) = attempt {
                    return Some(bytes);
                }
            }
            None
        })
    }
}
