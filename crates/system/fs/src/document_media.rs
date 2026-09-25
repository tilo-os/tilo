//! Document media in the content store.
//!
//! Images and attachments that app documents reference are ordinary blobs in
//! this daemon's [`ContentStore`], next to file content, and reach apps
//! through the scoped capability defined in [`runtime_fs::blobs`].
//!
//! # Reachability
//!
//! Possessing a hash never grants access. A blob is readable through an app's
//! capability only while this device's index records the app's room as
//! referencing it, which happens in exactly two ways:
//!
//! - an app in that room put the bytes on this device, or
//! - a collaborator's device, asked for that room and hash over the
//!   `tilo-fs-media/0` peer protocol, served the bytes because *its* index
//!   records the same room and the requester is a member of the room.
//!
//! Reachability is per room, not per app, because apps in a room hand content
//! to one another by hash (a typed action's content resource, a launch). Bytes
//! that happen to be in the store for another reason (a file, another room)
//! never satisfy a read: a room that lacks the reference must fetch from a
//! room member, which proves the content is reachable there.
//!
//! The index is a family of iroh-blobs tags, `tilo/media/v1/<room>/<hash>`.
//! A lookup is one exact key read, never a scan, and every reference is also a
//! GC root, so referenced media survives eviction sweeps that release only the
//! filesystem's own tags.
//!
//! # What this does not do yet
//!
//! References are recorded per scope when content is put or fetched; nothing
//! removes them, so document media is never reclaimed automatically. The
//! record store replaces this index with the reverse index from content to
//! the records whose retained history references it, which is also what lets
//! an app be limited to its own record kinds plus explicit grants, and a peer
//! be served by record rather than by room membership.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use iroh_blobs::api::blobs::BlobStatus;
use iroh_blobs::{BlobFormat, Hash, HashAndFormat};
use runtime_fs::blobs::{
    BlobBackend, BlobError, BlobFuture, BlobRef, BlobScope, MAX_BLOB_BYTES, ReadOutcome,
};

use crate::store::ContentStore;

#[cfg(feature = "filesystem")]
mod peer;
#[cfg(feature = "filesystem")]
pub(crate) use peer::accept_media_protocol;
#[cfg(feature = "filesystem")]
pub use peer::{PeerMediaFetcher, TILO_FS_MEDIA_ALPN, fetch_from_peer};

const TAG_PREFIX: &str = "tilo/media/v1/";
/// A failed fetch answers `unavailable` for this long before a read retries.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Concurrent peer fetches; further reads answer `pending` and ask again.
const MAX_IN_FLIGHT: usize = 32;
const MAX_TRACKED_FETCHES: usize = 4096;

fn reference_tag(scope: &BlobScope, hash: Hash) -> String {
    format!("{TAG_PREFIX}{}/{}", scope.room(), hash.to_hex())
}

/// Does this device's index record `scope`'s room as referencing `hash`?
pub(crate) async fn references(store: &ContentStore, scope: &BlobScope, hash: Hash) -> bool {
    matches!(
        store.store().tags().get(reference_tag(scope, hash)).await,
        Ok(Some(info)) if info.hash == hash && info.format == BlobFormat::Raw
    )
}

async fn record_reference(store: &ContentStore, scope: &BlobScope, hash: Hash) -> Result<()> {
    store
        .store()
        .tags()
        .set(reference_tag(scope, hash), HashAndFormat::raw(hash))
        .await?;
    store.store().sync_db().await?;
    Ok(())
}

/// Fetches a blob that a scope references on some collaborator's device.
pub trait MediaFetcher: Send + Sync + 'static {
    /// Verified bytes of `hash`, from any peer that serves it for `scope`.
    fn fetch(&self, scope: BlobScope, hash: Hash) -> BlobFuture<Option<Bytes>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FetchState {
    InFlight,
    Failed(Instant),
}

/// The daemon side of the document-media capability.
pub struct DocumentMedia {
    store: Arc<ContentStore>,
    fetcher: Option<Arc<dyn MediaFetcher>>,
    fetches: Mutex<HashMap<(String, Hash), FetchState>>,
}

impl DocumentMedia {
    /// Serve `store`; `fetcher` reaches collaborators, `None` keeps every
    /// read local.
    pub fn new(store: Arc<ContentStore>, fetcher: Option<Arc<dyn MediaFetcher>>) -> Arc<Self> {
        Arc::new(Self {
            store,
            fetcher,
            fetches: Mutex::new(HashMap::new()),
        })
    }

    /// Store `bytes` for `scope`. The whole payload is always hashed and
    /// imported before answering, so timing does not reveal prior presence.
    ///
    /// # Errors
    ///
    /// Returns an error above [`MAX_BLOB_BYTES`] or when the store fails.
    pub async fn store(&self, scope: &BlobScope, bytes: Vec<u8>) -> Result<Hash, BlobError> {
        let len = bytes.len() as u64;
        if len > MAX_BLOB_BYTES {
            return Err(BlobError::TooLarge {
                len,
                limit: MAX_BLOB_BYTES,
            });
        }
        let hash = self
            .store
            .ingest(Bytes::from(bytes))
            .await
            .map_err(|error| BlobError::Refused(error.to_string()))?;
        record_reference(&self.store, scope, hash)
            .await
            .map_err(|error| BlobError::Refused(error.to_string()))?;
        Ok(hash)
    }

    /// Whole content of a readable, resident blob.
    pub async fn resident(&self, scope: &BlobScope, hash: Hash) -> Option<Bytes> {
        if references(&self.store, scope, hash).await {
            self.store.local(hash).await
        } else {
            None
        }
    }

    /// Read a range, starting a peer fetch when this scope lacks the content.
    pub async fn read_range(
        self: &Arc<Self>,
        scope: BlobScope,
        hash: Hash,
        offset: u64,
        max_len: u32,
    ) -> ReadOutcome {
        if let Some(bytes) = self.resident(&scope, hash).await {
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            let end = start
                .saturating_add(usize::try_from(max_len).unwrap_or(usize::MAX))
                .min(bytes.len());
            return ReadOutcome::Ready {
                bytes: bytes.slice(start..end).to_vec(),
                total_len: bytes.len() as u64,
            };
        }
        self.begin_fetch(scope, hash)
    }

    fn begin_fetch(self: &Arc<Self>, scope: BlobScope, hash: Hash) -> ReadOutcome {
        let Some(remote) = self.fetcher.clone() else {
            return ReadOutcome::Unavailable;
        };
        let key = (scope.room().to_owned(), hash);
        {
            let mut tracked = self
                .fetches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match tracked.get(&key) {
                Some(FetchState::InFlight) => return ReadOutcome::Pending,
                Some(FetchState::Failed(at)) if at.elapsed() < RETRY_AFTER => {
                    return ReadOutcome::Unavailable;
                }
                _ => {}
            }
            let in_flight = tracked
                .values()
                .filter(|state| **state == FetchState::InFlight)
                .count();
            if in_flight >= MAX_IN_FLIGHT {
                return ReadOutcome::Pending;
            }
            if tracked.len() >= MAX_TRACKED_FETCHES {
                tracked.retain(|_, state| match state {
                    FetchState::InFlight => true,
                    FetchState::Failed(at) => at.elapsed() < RETRY_AFTER,
                });
                if tracked.len() >= MAX_TRACKED_FETCHES {
                    return ReadOutcome::Pending;
                }
            }
            tracked.insert(key.clone(), FetchState::InFlight);
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let stored = match remote.fetch(scope.clone(), hash).await {
                Some(bytes) => this.record_fetched(&scope, hash, bytes).await.is_ok(),
                None => false,
            };
            let mut tracked = this
                .fetches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if stored {
                tracked.remove(&key);
            } else {
                tracked.insert(key, FetchState::Failed(Instant::now()));
            }
        });
        ReadOutcome::Pending
    }

    async fn record_fetched(&self, scope: &BlobScope, hash: Hash, bytes: Bytes) -> Result<()> {
        self.store.store_fetched(hash, bytes).await?;
        record_reference(&self.store, scope, hash).await
    }

    /// Readable through `scope` and resident here.
    pub async fn contains(&self, scope: &BlobScope, hash: Hash) -> bool {
        references(&self.store, scope, hash).await && self.store.has(hash).await
    }

    /// Length of a readable, resident blob.
    pub async fn len_of(&self, scope: &BlobScope, hash: Hash) -> Option<u64> {
        if !references(&self.store, scope, hash).await {
            return None;
        }
        match self.store.store().blobs().status(hash).await {
            Ok(BlobStatus::Complete { size }) => Some(size),
            _ => None,
        }
    }
}

fn parse_hash(hex: &str) -> Option<Hash> {
    runtime_fs::blobs::valid_hash(hex)
        .then(|| hex.parse::<Hash>().ok())
        .flatten()
}

impl BlobBackend for DocumentMedia {
    fn put(
        self: Arc<Self>,
        scope: BlobScope,
        bytes: Vec<u8>,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>> {
        Box::pin(async move {
            let len = bytes.len() as u64;
            let hash = self.store(&scope, bytes).await?;
            Ok(BlobRef {
                hash: hash.to_hex(),
                len,
                mime,
            })
        })
    }

    fn read(
        self: Arc<Self>,
        scope: BlobScope,
        hash: String,
        offset: u64,
        max_len: u32,
    ) -> BlobFuture<ReadOutcome> {
        Box::pin(async move {
            match parse_hash(&hash) {
                Some(hash) => self.read_range(scope, hash, offset, max_len).await,
                None => ReadOutcome::Unavailable,
            }
        })
    }

    fn has(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<bool> {
        Box::pin(async move {
            match parse_hash(&hash) {
                Some(hash) => self.contains(&scope, hash).await,
                None => false,
            }
        })
    }

    fn size(self: Arc<Self>, scope: BlobScope, hash: String) -> BlobFuture<Option<u64>> {
        Box::pin(async move {
            match parse_hash(&hash) {
                Some(hash) => self.len_of(&scope, hash).await,
                None => None,
            }
        })
    }
}

/// Bind the local capability socket and serve it on its own thread.
///
/// The socket is group-connectable (`0660`); its directory decides who may
/// reach it, and the launcher is the only thing that hands a connection to a
/// sandboxed app.
///
/// # Errors
///
/// Returns an error when the socket cannot be bound or the server thread
/// cannot start.
pub fn serve_socket(
    socket: &Path,
    media: Arc<DocumentMedia>,
) -> Result<std::thread::JoinHandle<()>> {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_dir() => {
            anyhow::bail!("{} is not a socket path", socket.display());
        }
        Ok(_) => std::fs::remove_file(socket)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = std::os::unix::net::UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660))?;
    let handle = runtime_fs::blobs::spawn_blob_server(
        listener,
        media as Arc<dyn BlobBackend>,
        tokio::runtime::Handle::current(),
    )?;
    println!("Document media socket at {}", socket.display());
    Ok(handle)
}

#[cfg(all(test, feature = "filesystem"))]
mod tests;
