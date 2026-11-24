//! Document media in the content store.
//!
//! Images and attachments that app documents reference are ordinary blobs in
//! this daemon's [`ContentStore`], next to file content, and reach apps
//! through the scoped capability defined in [`runtime_fs::blobs`].
//!
//! # Reachability
//!
//! Possessing a hash never grants access. A blob is readable through an app's
//! capability only while this device's index records the app's workspace as
//! referencing it, which happens when the bytes were put on this device for
//! that workspace: by an app in it, or by the session for a record of the
//! workspace whose content arrived from another of the record's devices. That
//! exchange is the collaboration broker's — content travels on the record's
//! route (`tilo_workspace_service::content`), never between daemons.
//!
//! Reachability is per workspace, not per app, because apps in one hand content
//! to one another by hash (a typed action's content resource, a launch). Bytes
//! that happen to be in the store for another reason (a file, another
//! workspace) never satisfy a read: a workspace that lacks the reference must
//! get the bytes
//! from a device of the record that references them.
//!
//! The index is a family of iroh-blobs tags, `tilo/media/v1/<workspace>/<hash>`.
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
//! be served by record rather than by workspace membership.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use bytes::Bytes;
use futures_util::{Stream, StreamExt as _};
use iroh_blobs::Hash;
use iroh_blobs::api::blobs::BlobStatus;
use runtime_fs::blobs::{
    BlobBackend, BlobChunks, BlobError, BlobFuture, BlobRef, BlobScope, MAX_BLOB_BYTES, PublicRead,
    ReadOutcome,
};

use crate::public_content::PublicContent;
use crate::store::ContentStore;

const TAG_PREFIX: &str = "tilo/media/v1/";

fn reference_tag(scope: &BlobScope, hash: Hash) -> String {
    format!("{TAG_PREFIX}{}/{}", scope.workspace(), hash.to_hex())
}

/// Does this device's index record `scope`'s workspace as referencing `hash`?
async fn references(store: &ContentStore, scope: &BlobScope, hash: Hash) -> bool {
    store.reference(&reference_tag(scope, hash)).await == Some(hash)
}

async fn document_reference(store: &ContentStore, scope: &BlobScope, hash: Hash) -> Result<()> {
    document_reference_tag(store, reference_tag(scope, hash), hash).await
}

async fn document_reference_tag(store: &ContentStore, tag: String, hash: Hash) -> Result<()> {
    store.set_reference(&tag, hash).await
}

/// Fetches content this device does not keep, given the scope that references it and its address.
type ContentFetcher = Arc<dyn Fn(&BlobScope, &str) + Send + Sync>;
/// The content secret the members of a scope's workspace hold, when this device holds it.
pub type ContentSecrets = Arc<dyn Fn(&BlobScope) -> Option<tilo_seal::SymmetricKey> + Send + Sync>;
/// When each open scope's address was last asked for.
type AskedAt = std::collections::HashMap<(BlobScope, String), std::time::Instant>;

pub struct DocumentMedia {
    store: Arc<ContentStore>,
    /// The public content this device keeps, where it keeps any.
    public: Option<Arc<PublicContent>>,
    /// What a session's ask for content this device does not keep reaches: the host's record
    /// content, which asks the workspace's other devices and keeps what they answer here, where
    /// the session's next read finds it.
    fetching: std::sync::RwLock<Option<ContentFetcher>>,
    /// The members' content secret a part this device seals for a session derives its key from:
    /// the device's daemon holds the workspace's keys, and its sessions do not.
    secrets: std::sync::RwLock<Option<ContentSecrets>>,
    /// When each scope's address was last asked for among those open: a session reads again as it
    /// waits, and the host is asked once a moment rather than once a read.
    asked: std::sync::Mutex<AskedAt>,
}

/// How long after a want the same address is asked for again: a session's wait reads it often,
/// and the host asks the workspace's devices on its own patience.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_millis(500);
/// Wants this device remembers to keep from asking again; past it, they are forgotten.
const ASKED_KEPT: usize = 1024;

impl DocumentMedia {
    pub fn new(store: Arc<ContentStore>) -> Arc<Self> {
        Arc::new(Self {
            store,
            public: None,
            fetching: std::sync::RwLock::new(None),
            secrets: std::sync::RwLock::new(None),
            asked: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Serve `store` and the public content this device keeps in it.
    pub fn with_public(store: Arc<ContentStore>, public: Arc<PublicContent>) -> Arc<Self> {
        Arc::new(Self {
            store,
            public: Some(public),
            fetching: std::sync::RwLock::new(None),
            secrets: std::sync::RwLock::new(None),
            asked: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Answer a session's ask for content this device lacks through `fetch` from now on: the
    /// host's record content, which asks the workspace's other devices for it and keeps what they
    /// answer on this device's store. Until a host installs one, a session's ask starts nothing.
    pub fn fetch_through(&self, fetch: ContentFetcher) {
        if let Ok(mut held) = self.fetching.write() {
            *held = Some(fetch);
        }
    }

    /// Seal the parts a session keeps under keys `secrets` derives from the members' secret from
    /// now on: the same bytes kept twice are then one item under one reference. Until a host
    /// installs one, each part is sealed under a fresh key.
    pub fn seal_with(&self, secrets: ContentSecrets) {
        if let Ok(mut held) = self.secrets.write() {
            *held = Some(secrets);
        }
    }

    /// The members' content secret of `scope`'s workspace, when the host holds it.
    fn secret(&self, scope: &BlobScope) -> Option<tilo_seal::SymmetricKey> {
        let secrets = self.secrets.read().ok()?.clone()?;
        secrets(scope)
    }

    /// Start fetching `hash` of `scope` from elsewhere, when a host fetches at all, once a moment:
    /// the session that asked reads again as it waits.
    fn started_fetching(&self, scope: &BlobScope, hash: &str) {
        let fetch = self
            .fetching
            .read()
            .ok()
            .and_then(|held| held.as_ref().map(Arc::clone));
        let Some(fetch) = fetch else {
            return;
        };
        {
            let Ok(mut asked) = self.asked.lock() else {
                return;
            };
            let key = (scope.clone(), hash.to_owned());
            let now = std::time::Instant::now();
            if let Some(last) = asked.get(&key)
                && now.duration_since(*last) < ASK_AGAIN
            {
                return;
            }
            if asked.len() >= ASKED_KEPT {
                asked.clear();
            }
            asked.insert(key, now);
        }
        fetch(scope, hash);
    }

    fn public(&self) -> Result<&Arc<PublicContent>, BlobError> {
        self.public
            .as_ref()
            .ok_or_else(|| BlobError::Refused("this device keeps no public content".into()))
    }

    /// Store `bytes` for `scope`. The whole payload is always hashed and
    /// imported before answering, so timing does not reveal prior presence.
    pub async fn store(&self, scope: &BlobScope, bytes: Vec<u8>) -> Result<Hash, BlobError> {
        let len = bytes.len() as u64;
        if len > MAX_BLOB_BYTES {
            return Err(BlobError::TooLarge {
                len,
                limit: MAX_BLOB_BYTES,
            });
        }
        let whole: std::io::Result<Bytes> = Ok(Bytes::from(bytes));
        let (hash, _) = self
            .store_stream(scope, futures_util::stream::iter([whole]))
            .await?;
        Ok(hash)
    }

    /// Store content that arrives in pieces for `scope`, hashing and
    /// importing each piece as it comes, so the daemon never holds the whole
    /// content, and never caching it: media is read back by range. As for
    /// [`Self::store`], all of it is hashed and imported before answering,
    /// and the workspace's reference is recorded only once the content is
    /// durable. Returns the content's hash and length.
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::TooLarge`] as soon as the pieces pass
    /// [`MAX_BLOB_BYTES`], and a refusal when a piece is an error (the
    /// upload was abandoned) or the store fails. Nothing is stored then.
    pub async fn store_stream<S>(
        &self,
        scope: &BlobScope,
        pieces: S,
    ) -> Result<(Hash, u64), BlobError>
    where
        S: Stream<Item = std::io::Result<Bytes>> + Send + Sync + 'static,
    {
        self.store_stream_within(scope, pieces, Some(MAX_BLOB_BYTES))
            .await
    }

    /// Store content that arrives in pieces for `scope` with no document-media
    /// bound: a file's bytes, which a transfer carries whole onto disk before
    /// they are kept, and which belong in a file record rather than in the
    /// media capability's size. Streamed and referenced exactly as
    /// [`Self::store_stream`] does, and never counted against the media
    /// store's bound.
    ///
    /// # Errors
    ///
    /// A refusal when a piece is an error (the upload was abandoned) or the
    /// store fails. Nothing is stored then.
    pub async fn store_file_stream<S>(
        &self,
        scope: &BlobScope,
        pieces: S,
    ) -> Result<(Hash, u64), BlobError>
    where
        S: Stream<Item = std::io::Result<Bytes>> + Send + Sync + 'static,
    {
        self.store_stream_within(scope, pieces, None).await
    }

    async fn store_stream_within<S>(
        &self,
        scope: &BlobScope,
        pieces: S,
        limit: Option<u64>,
    ) -> Result<(Hash, u64), BlobError>
    where
        S: Stream<Item = std::io::Result<Bytes>> + Send + Sync + 'static,
    {
        let seen = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&seen);
        let limited = pieces.map(move |piece| {
            let piece = piece?;
            let len = counted
                .fetch_add(piece.len() as u64, Ordering::Relaxed)
                .saturating_add(piece.len() as u64);
            if limit.is_some_and(|limit| len > limit) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "document media above the size limit",
                ));
            }
            Ok(piece)
        });
        let (hash, len) = match self.store.ingest_stream(limited).await {
            Ok(stored) => stored,
            Err(error) => {
                let len = seen.load(Ordering::Relaxed);
                return Err(match limit {
                    Some(limit) if len > limit => BlobError::TooLarge { len, limit },
                    _ => BlobError::Refused(error.to_string()),
                });
            }
        };
        document_reference(&self.store, scope, hash)
            .await
            .map_err(|error| BlobError::Refused(error.to_string()))?;
        Ok((hash, len))
    }

    /// Read a range; content this scope does not reference here is
    /// unavailable.
    pub async fn read_range(
        &self,
        scope: BlobScope,
        hash: Hash,
        offset: u64,
        max_len: u32,
    ) -> ReadOutcome {
        if !references(&self.store, &scope, hash).await {
            return ReadOutcome::Unavailable;
        }
        let end = offset.saturating_add(u64::from(max_len));
        match self.store.read_blob_range(hash, offset..end).await {
            Some((bytes, total_len)) => ReadOutcome::Ready { bytes, total_len },
            None => ReadOutcome::Unavailable,
        }
    }

    pub async fn contains(&self, scope: &BlobScope, hash: Hash) -> bool {
        references(&self.store, scope, hash).await && self.store.has(hash).await
    }

    pub async fn len_of(&self, scope: &BlobScope, hash: Hash) -> Option<u64> {
        if !references(&self.store, scope, hash).await {
            return None;
        }
        match self.store.blob_status(hash).await {
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

    fn put_stream(
        self: Arc<Self>,
        scope: BlobScope,
        mut chunks: BlobChunks,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>> {
        Box::pin(async move {
            let pieces = futures_util::stream::poll_fn(move |cx| {
                chunks
                    .poll_next(cx)
                    .map(|piece| piece.map(|piece| piece.map(Bytes::from)))
            });
            let (hash, len) = self.store_stream(&scope, pieces).await?;
            Ok(BlobRef {
                hash: hash.to_hex(),
                len,
                mime,
            })
        })
    }

    fn put_sealed_stream(
        self: Arc<Self>,
        scope: BlobScope,
        mut chunks: BlobChunks,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>> {
        Box::pin(async move {
            // The key derives from the whole plaintext, so it is held before it is sealed: a
            // part is at most MAX_BLOB_BYTES.
            let mut bytes = Vec::new();
            while let Some(piece) = chunks.next().await {
                let piece = piece.map_err(|error| BlobError::Refused(error.to_string()))?;
                bytes.extend_from_slice(&piece);
                if bytes.len() as u64 > MAX_BLOB_BYTES {
                    return Err(BlobError::TooLarge {
                        len: bytes.len() as u64,
                        limit: MAX_BLOB_BYTES,
                    });
                }
            }
            let key = match self.secret(&scope) {
                Some(secret) => tilo_content::part_key(&secret, &bytes),
                None => tilo_seal::SymmetricKey::generate()
                    .map_err(|error| BlobError::Refused(error.to_string()))?,
            };
            let len = bytes.len() as u64;
            let sealed = tilo_seal::content::seal(&key, &bytes)
                .map_err(|error| BlobError::Refused(error.to_string()))?;
            let hash = self.store(&scope, sealed).await?;
            Ok(BlobRef {
                hash: tilo_content::ContentRef::sealed(&hash.to_hex(), key).to_string(),
                len,
                mime,
            })
        })
    }

    fn put_file_stream(
        self: Arc<Self>,
        scope: BlobScope,
        mut chunks: BlobChunks,
        mime: String,
    ) -> BlobFuture<Result<BlobRef, BlobError>> {
        Box::pin(async move {
            let pieces = futures_util::stream::poll_fn(move |cx| {
                chunks
                    .poll_next(cx)
                    .map(|piece| piece.map(|piece| piece.map(Bytes::from)))
            });
            let (hash, len) = self.store_file_stream(&scope, pieces).await?;
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

    fn public_keep(
        self: Arc<Self>,
        scope: BlobScope,
        name: String,
    ) -> BlobFuture<Result<(), BlobError>> {
        Box::pin(async move { self.public()?.keep(&name, scope.app()) })
    }

    fn public_release(
        self: Arc<Self>,
        scope: BlobScope,
        name: String,
    ) -> BlobFuture<Result<(), BlobError>> {
        Box::pin(async move { self.public()?.release(&name, scope.app()) })
    }

    fn public_read(
        self: Arc<Self>,
        name: String,
        offset: u64,
        max_len: u32,
    ) -> BlobFuture<Result<PublicRead, BlobError>> {
        Box::pin(async move {
            let public = Arc::clone(self.public()?);
            // Reading touches the disk.
            tokio::task::spawn_blocking(move || public.read(&name, offset, max_len))
                .await
                .map_err(|error| BlobError::Refused(error.to_string()))?
        })
    }

    fn refresh_public(self: Arc<Self>) -> BlobFuture<u32> {
        Box::pin(async move {
            match self.public.clone() {
                Some(public) => public.refresh().await,
                None => 0,
            }
        })
    }

    fn fetching(&self, scope: &BlobScope, hash: &str) {
        self.started_fetching(scope, hash);
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
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_dir() => {
            anyhow::bail!("{} is not a socket path", socket.display());
        }
        Ok(_) => std::fs::remove_file(socket)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = tilo_files::native::socket::bind(socket)?;
    // A Unix socket is reached by whoever its mode lets in; a named pipe is this account's alone
    // from the moment it is made.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o660))?;
    }
    let handle = runtime_fs::blobs::spawn_blob_server(
        listener,
        media as Arc<dyn BlobBackend>,
        tokio::runtime::Handle::current(),
    )?;
    println!("Document media socket at {}", socket.display());
    Ok(handle)
}

#[cfg(test)]
#[cfg(feature = "filesystem")]
mod tests;
