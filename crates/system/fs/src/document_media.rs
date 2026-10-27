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
//! referencing it, which happens when the bytes were put on this device for
//! that room: by an app in the room, or by the session for a record of the
//! room whose content arrived from another of the record's devices. That
//! exchange is the collaboration broker's — content travels on the record's
//! route (`tilo_action_broker::record_content`), never between daemons.
//!
//! A host can also offer an explicit owner action through
//! [`LegacyMediaImportHost`]: it verifies a bounded selection and creates new
//! room references, without claiming historical ownership. That host-only
//! authority is not exposed on the ordinary blob capability or socket.
//!
//! Reachability is per room, not per app, because apps in a room hand content
//! to one another by hash (a typed action's content resource, a launch). Bytes
//! that happen to be in the store for another reason (a file, another room)
//! never satisfy a read: a room that lacks the reference must get the bytes
//! from a device of the record that references them.
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

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use bytes::Bytes;
use futures_util::{Stream, StreamExt as _};
use iroh_blobs::api::blobs::BlobStatus;
use iroh_blobs::{BlobFormat, Hash, HashAndFormat};
use runtime_fs::blobs::{
    BlobBackend, BlobChunks, BlobError, BlobFuture, BlobRef, BlobScope, MAX_BLOB_BYTES, PublicRead,
    ReadOutcome,
};

use crate::public_content::PublicContent;
use crate::store::ContentStore;

mod import;
pub use import::{
    LegacyImportApproval, LegacyImportAuthority, LegacyImportPermit, LegacyImportRequest,
    LegacyMediaImportHost, MAX_LEGACY_IMPORT_BYTES, MAX_LEGACY_IMPORT_ITEMS,
    MAX_LEGACY_IMPORT_LIFETIME,
};

const TAG_PREFIX: &str = "tilo/media/v1/";

fn reference_tag(scope: &BlobScope, hash: Hash) -> String {
    format!("{TAG_PREFIX}{}/{}", scope.room(), hash.to_hex())
}

/// Does this device's index record `scope`'s room as referencing `hash`?
async fn references(store: &ContentStore, scope: &BlobScope, hash: Hash) -> bool {
    matches!(
        store.store().tags().get(reference_tag(scope, hash)).await,
        Ok(Some(info)) if info.hash == hash && info.format == BlobFormat::Raw
    )
}

async fn record_reference(store: &ContentStore, scope: &BlobScope, hash: Hash) -> Result<()> {
    record_reference_tag(store, reference_tag(scope, hash), hash).await
}

async fn record_reference_tag(store: &ContentStore, tag: String, hash: Hash) -> Result<()> {
    store
        .store()
        .tags()
        .set(tag, HashAndFormat::raw(hash))
        .await?;
    store.store().sync_db().await?;
    Ok(())
}

/// The daemon side of the document-media capability.
pub struct DocumentMedia {
    store: Arc<ContentStore>,
    /// The public content this device keeps, where it keeps any.
    public: Option<Arc<PublicContent>>,
    /// The mount's write attribution leases and its mount point, which the
    /// session reaches through `SessionBlobs.attributeWrites`.
    attributions: Option<(
        Arc<crate::write_attribution::WriteAttributions>,
        std::path::PathBuf,
    )>,
}

impl DocumentMedia {
    /// Serve `store`.
    pub fn new(store: Arc<ContentStore>) -> Arc<Self> {
        Arc::new(Self {
            store,
            public: None,
            attributions: None,
        })
    }

    /// Serve `store` and the public content this device keeps in it.
    pub fn with_public(store: Arc<ContentStore>, public: Arc<PublicContent>) -> Arc<Self> {
        Arc::new(Self {
            store,
            public: Some(public),
            attributions: None,
        })
    }

    fn public(&self) -> Result<&Arc<PublicContent>, BlobError> {
        self.public
            .as_ref()
            .ok_or_else(|| BlobError::Refused("this device keeps no public content".into()))
    }

    /// Let the session lease write attributions for the workspace mounted at
    /// the given mount point ([`crate::write_attribution`]).
    #[must_use]
    pub fn with_write_attributions(
        self: Arc<Self>,
        attributions: (
            Arc<crate::write_attribution::WriteAttributions>,
            std::path::PathBuf,
        ),
    ) -> Arc<Self> {
        Arc::new(Self {
            store: Arc::clone(&self.store),
            public: self.public.clone(),
            attributions: Some(attributions),
        })
    }

    /// The session names the agent behind this device's next writes to some
    /// absolute paths under the mount. Only the session root reaches this.
    ///
    /// # Errors
    ///
    /// Refuses when this daemon mounts no workspace, a path lies outside the
    /// mount, or the lease is invalid.
    pub fn lease_write_attribution(
        &self,
        attribution: runtime_fs::blobs::WriteAttribution,
    ) -> Result<(), BlobError> {
        let Some((leases, mount_point)) = &self.attributions else {
            return Err(BlobError::Refused(
                "this filesystem attributes no writes".into(),
            ));
        };
        let paths = attribution
            .paths
            .iter()
            .map(|path| crate::write_attribution::mount_relative(mount_point, path))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| BlobError::Refused("an attributed path is outside the mount".into()))?;
        leases
            .grant(
                paths,
                fs_protocol::FileAttributionV1 {
                    actor: attribution.actor,
                    label: attribution.label,
                    color: attribution.color,
                },
                attribution.ttl,
            )
            .map_err(|error| BlobError::Refused(error.to_string()))
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
    /// and the room's reference is recorded only once the content is
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
        let seen = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&seen);
        let limited = pieces.map(move |piece| {
            let piece = piece?;
            let len = counted
                .fetch_add(piece.len() as u64, Ordering::Relaxed)
                .saturating_add(piece.len() as u64);
            if len > MAX_BLOB_BYTES {
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
                return Err(if len > MAX_BLOB_BYTES {
                    BlobError::TooLarge {
                        len,
                        limit: MAX_BLOB_BYTES,
                    }
                } else {
                    BlobError::Refused(error.to_string())
                });
            }
        };
        record_reference(&self.store, scope, hash)
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

    fn attribute_writes(
        &self,
        attribution: runtime_fs::blobs::WriteAttribution,
    ) -> Result<(), BlobError> {
        self.lease_write_attribution(attribution)
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

#[cfg(test)]
#[cfg(feature = "filesystem")]
mod tests;
