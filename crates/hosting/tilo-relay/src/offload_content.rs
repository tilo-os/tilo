//! The requester's workspace content, as an offloaded job reads and writes
//! it.
//!
//! Two content addresses meet here. The workspace filesystem names a file
//! version by its *manifest* hash: blake3 of the chunk list that describes
//! the bytes ([`fs::content::Manifest::id`]). A compute job names its inputs
//! and outputs by blake3 of the *bytes* themselves
//! ([`ComputeJobSpec::inputs`](crate::ComputeJobSpec::inputs)). Neither can
//! be derived from the other without the bytes, so the bridge never guesses:
//!
//! - **Inputs.** The invoker binds each input by the manifest hash its own
//!   view of the workspace resolved ([`WorkspaceContent::bind_file`]). The
//!   bytes are read from this device's content store, checked against that
//!   manifest, and hashed; only then is their digest servable. A digest
//!   nobody bound is unknown here, whatever it names elsewhere in the store,
//!   so a worker reads exactly what the invoking device chose to hand it and
//!   nothing it could not read itself.
//! - **Outputs.** A digest arriving with bytes is checked against them and
//!   written as a new file version ([`ComputeContentStore::put`]); its
//!   manifest hash is then known ([`WorkspaceContent::manifest_of`]), so
//!   the result lands in the workspace like any other file content.
//!
//! - **Document media.** Content an app put for its room is a raw blob whose
//!   hash *is* the digest of its bytes. It is bound by that digest through
//!   the invoking app's scope in its room ([`WorkspaceContent::bind_blob`]),
//!   read wherever the session keeps that media ([`RoomMedia`]), and is
//!   servable from then on, like a bound file — always re-read through the
//!   same scope. A run's outputs are kept as that scope's media
//!   ([`WorkspaceContent::keep_blob`]).
//!
//! Every read is re-checked: a bound file whose bytes no longer hash to the
//! bound digest is refused, never substituted.
//!
//! - **Inputs a worker already holds.** The same content on a *worker*
//!   device is [`HeldContent`]: when the invoker proves it holds an input,
//!   it names the file version as the proof's locator
//!   ([`ComputeContentStore::locator`]), and a worker whose own replica has
//!   that version whole runs the job on it instead of fetching the bytes.
//!   On an always-on device that replica is the filesystem's home-peer
//!   replica, read from beside it ([`ReplicaContent`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fs::document_media::DocumentMedia;
use fs::store::ContentStore;
use runtime_fs::blobs::BlobScope;
use thiserror::Error;

use crate::compute::{ComputeContentStore, HeldContent};
use crate::offload_media::RoomMedia;
use tilo_lock::MutexRecover as _;

/// Why a workspace file could not be bound as a job input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BindError {
    /// This device holds no manifest under that hash.
    #[error("no workspace content under that manifest")]
    UnknownManifest,
    /// The manifest is known but not every byte is on this device yet.
    #[error("the file's content is not on this device yet")]
    NotLocal,
    /// The local bytes do not match the manifest.
    #[error("the file's local content does not match its manifest")]
    Integrity,
    /// Where the rooms' media is kept did not answer.
    #[error("the room's media: {0}")]
    Media(String),
}

/// One device's workspace content, addressed by both hashes for the jobs it
/// offloads. Cheap to clone; clones share their bindings.
#[derive(Clone)]
pub struct WorkspaceContent {
    /// Workspace files, when this device keeps them in this process.
    store: Option<Arc<ContentStore>>,
    /// The rooms' document media.
    media: Arc<dyn RoomMedia>,
    runtime: tokio::runtime::Handle,
    /// Byte digest → manifest hash, for every input bound and every output
    /// stored through this handle.
    bound: Arc<Mutex<HashMap<[u8; 32], [u8; 32]>>>,
    /// Content bound as document media, by the digest that is its blob
    /// hash, and the scope it is read through.
    blobs: Arc<Mutex<HashMap<[u8; 32], BlobScope>>>,
}

impl WorkspaceContent {
    /// The content of `store`, whose document media is the rooms' too. The
    /// synchronous [`ComputeContentStore`] side runs its reads on `runtime`
    /// and must be called off it (from `spawn_blocking`), as the compute
    /// paths do.
    #[must_use]
    pub fn new(store: Arc<ContentStore>, runtime: tokio::runtime::Handle) -> Self {
        let media = DocumentMedia::new(Arc::clone(&store));
        Self {
            store: Some(store),
            media,
            runtime,
            bound: Arc::default(),
            blobs: Arc::default(),
        }
    }

    /// Only the rooms' document media in `media`, for a session whose
    /// workspace files are not in this process: no file version binds, and
    /// nothing is held for a worker. As [`Self::new`] otherwise.
    #[must_use]
    pub fn of_media(media: Arc<dyn RoomMedia>, runtime: tokio::runtime::Handle) -> Self {
        Self {
            store: None,
            media,
            runtime,
            bound: Arc::default(),
            blobs: Arc::default(),
        }
    }

    /// Bind document media `scope`'s room reaches on this device — content
    /// an app put for its room, named by the blake3 of its bytes, which is
    /// also the digest a job names it by — as a job input, and return how
    /// many bytes it is. It is read, now and whenever a worker fetches it,
    /// only through `scope`.
    ///
    /// # Errors
    ///
    /// [`BindError::NotLocal`] when `scope` does not reach it wholly here,
    /// [`BindError::Integrity`] when its bytes do not hash to `digest`, and
    /// [`BindError::Media`] when the media did not answer.
    pub async fn bind_blob(&self, scope: &BlobScope, digest: [u8; 32]) -> Result<u64, BindError> {
        let bytes = self.media.read(scope, digest).await?;
        self.blobs.lock_recover().insert(digest, scope.clone());
        Ok(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
    }

    /// How many bytes the document media `digest` is, when `scope`'s room
    /// reaches it here: its recorded length, without reading or checking
    /// the bytes.
    ///
    /// # Errors
    ///
    /// [`BindError::NotLocal`] when `scope` does not reach it, or
    /// [`BindError::Media`] when the media did not answer; binds nothing.
    pub async fn blob_len(&self, scope: &BlobScope, digest: [u8; 32]) -> Result<u64, BindError> {
        self.media.len(scope, digest).await
    }

    /// Keep `bytes`, which hash to `digest`, as `scope`'s document media,
    /// bound from then on like an input.
    ///
    /// # Errors
    ///
    /// A diagnostic when the bytes do not match `digest` or cannot be kept.
    pub async fn keep_blob(
        &self,
        scope: &BlobScope,
        digest: [u8; 32],
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        if blake3::hash(&bytes).as_bytes() != &digest {
            return Err("content does not match its digest".into());
        }
        if self.media.keep(scope, bytes).await? != digest {
            return Err("the content store keeps these bytes under another address".into());
        }
        self.blobs.lock_recover().insert(digest, scope.clone());
        Ok(())
    }

    fn blob_scope(&self, digest: [u8; 32]) -> Option<BlobScope> {
        self.blobs.lock_recover().get(&digest).cloned()
    }

    fn bindings(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], [u8; 32]>> {
        self.bound.lock_recover()
    }

    /// Bind the file version `manifest` as a job input and return the
    /// digest a job names it by.
    ///
    /// # Errors
    ///
    /// [`BindError`] when the content is unknown, not wholly local, or does
    /// not match its manifest.
    pub async fn bind_file(&self, manifest: [u8; 32]) -> Result<[u8; 32], BindError> {
        Ok(self.bind_input(manifest).await?.0)
    }

    /// [`Self::bind_file`], also returning how many bytes the input is.
    ///
    /// # Errors
    ///
    /// As [`Self::bind_file`].
    pub async fn bind_input(&self, manifest: [u8; 32]) -> Result<([u8; 32], u64), BindError> {
        let bytes = read_version(self.store.as_deref(), manifest).await?;
        let digest = *blake3::hash(&bytes).as_bytes();
        self.bindings().insert(digest, manifest);
        Ok((digest, u64::try_from(bytes.len()).unwrap_or(u64::MAX)))
    }

    /// The manifest hash of content this handle bound or stored under
    /// `digest`.
    #[must_use]
    pub fn manifest_of(&self, digest: [u8; 32]) -> Option<[u8; 32]> {
        self.bindings().get(&digest).copied()
    }

    /// The bytes of bound `digest`, re-read and re-checked.
    ///
    /// # Errors
    ///
    /// A diagnostic when the bound file can no longer be read as those bytes.
    pub async fn read(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
        let Some(manifest) = self.manifest_of(digest) else {
            if let Some(scope) = self.blob_scope(digest) {
                return self
                    .media
                    .read(&scope, digest)
                    .await
                    .map(Some)
                    .map_err(|error| error.to_string());
            }
            return Ok(None);
        };
        let bytes = read_version(self.store.as_deref(), manifest)
            .await
            .map_err(|error| error.to_string())?;
        if blake3::hash(&bytes).as_bytes() == &digest {
            Ok(Some(bytes))
        } else {
            Err("bound workspace content changed under its digest".into())
        }
    }

    /// Store `bytes`, which hash to `digest`, as a file version, and return
    /// its manifest hash.
    ///
    /// # Errors
    ///
    /// A diagnostic when the bytes do not match `digest`, are empty (an
    /// empty file has no version to name), or cannot be stored.
    pub async fn store(&self, digest: [u8; 32], bytes: &[u8]) -> Result<[u8; 32], String> {
        if blake3::hash(bytes).as_bytes() != &digest {
            return Err("content does not match its digest".into());
        }
        let manifest = self
            .store
            .as_ref()
            .ok_or("this session keeps no workspace files here")?
            .write_content(bytes)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "empty content has no file version".to_string())?;
        let manifest = *manifest.as_bytes();
        self.bindings().insert(digest, manifest);
        Ok(manifest)
    }

    /// Run `future` on the runtime from a blocking thread.
    fn block<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = T> + Send + 'static,
    ) -> Result<T, String> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        self.runtime.spawn(async move {
            let _ = sender.send(future.await);
        });
        receiver
            .recv()
            .map_err(|_| "workspace content runtime stopped".to_string())
    }
}

/// The whole of file version `manifest`, from `store`, checked against it.
async fn read_version(
    store: Option<&ContentStore>,
    manifest: [u8; 32],
) -> Result<Vec<u8>, BindError> {
    let store = store.ok_or(BindError::UnknownManifest)?;
    read_version_within(store, manifest, u64::MAX)
        .await?
        .ok_or(BindError::NotLocal)
}

/// [`read_version`], or `Ok(None)` without reading when the version is
/// longer than `max_bytes`.
async fn read_version_within(
    store: &ContentStore,
    manifest: [u8; 32],
    max_bytes: u64,
) -> Result<Option<Vec<u8>>, BindError> {
    let decoded = store
        .manifest(manifest.into())
        .await
        .ok_or(BindError::UnknownManifest)?;
    if decoded.total_len() > max_bytes {
        return Ok(None);
    }
    let bytes = store
        .read_local(manifest.into(), &decoded, 0..decoded.total_len())
        .await
        .ok_or(BindError::NotLocal)?;
    if decoded.verify(&bytes) {
        Ok(Some(bytes))
    } else {
        Err(BindError::Integrity)
    }
}

impl ComputeContentStore for WorkspaceContent {
    fn get(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
        let this = self.clone();
        self.block(async move { this.read(digest).await })?
    }

    fn put(&self, digest: [u8; 32], bytes: &[u8]) -> Result<(), String> {
        let this = self.clone();
        let bytes = bytes.to_vec();
        self.block(async move { this.store(digest, &bytes).await.map(|_| ()) })?
    }

    fn locator(&self, digest: [u8; 32]) -> Option<[u8; 32]> {
        self.manifest_of(digest)
    }
}

impl HeldContent for WorkspaceContent {
    fn held(
        &self,
        digest: [u8; 32],
        locator: Option<[u8; 32]>,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(manifest) = locator.or_else(|| self.manifest_of(digest)) else {
            return Ok(None);
        };
        // A session without workspace files here holds none for a worker.
        let Some(store) = self.store.clone() else {
            return Ok(None);
        };
        let read =
            self.block(async move { read_version_within(&store, manifest, max_bytes).await })?;
        match read {
            // A version this replica lacks, or holds only in part, is simply
            // not held: the input is fetched instead.
            Err(BindError::UnknownManifest | BindError::NotLocal) => Ok(None),
            Err(error @ (BindError::Integrity | BindError::Media(_))) => Err(error.to_string()),
            Ok(bytes) => Ok(bytes.filter(|bytes| blake3::hash(bytes).as_bytes() == &digest)),
        }
    }

    fn locates(&self) -> bool {
        true
    }
}

/// The workspace content an always-on device's filesystem replica keeps
/// (`fs replica`, one store per home-peer workspace), as [`HeldContent`]
/// for the compute worker running beside it.
///
/// The replica process holds its stores open, so this reads their
/// file-backed versions without opening them
/// ([`ContentStore::read_file_version_unlocked`]), and asks the replica for
/// smaller ones, which are loose blobs inside its database, over the
/// owner-only socket each workspace's replica serves in its own cache
/// directory ([`fs::replica_content`]). A version is found only by the
/// requester's locator — the manifest hash it proved its copy under — and
/// returned only when its bytes hash to the input digest: a partly hydrated,
/// replaced, foreign or misreported version is simply not held, and the
/// input is fetched instead.
#[derive(Clone, Debug)]
pub struct ReplicaContent {
    cache_dirs: Vec<std::path::PathBuf>,
}

impl ReplicaContent {
    /// The replica stores at `cache_dirs`.
    #[must_use]
    pub fn new(cache_dirs: Vec<std::path::PathBuf>) -> Self {
        Self { cache_dirs }
    }
}

impl HeldContent for ReplicaContent {
    fn held(
        &self,
        digest: [u8; 32],
        locator: Option<[u8; 32]>,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        // Without the requester's file version there is nothing to look up:
        // a digest alone names no file here.
        let Some(manifest) = locator else {
            return Ok(None);
        };
        let mut failure = None;
        for dir in &self.cache_dirs {
            let read = match ContentStore::read_file_version_unlocked(dir, manifest, max_bytes) {
                Ok(None) => fs::replica_content::read(dir, manifest, max_bytes),
                file => file,
            };
            match read {
                Ok(Some(bytes)) if blake3::hash(&bytes).as_bytes() == &digest => {
                    return Ok(Some(bytes));
                }
                Ok(_) => {}
                Err(error) => failure = Some(error.to_string()),
            }
        }
        failure.map_or(Ok(None), Err)
    }

    fn locates(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_bound_files_are_served_and_outputs_become_file_versions() {
        let store = Arc::new(ContentStore::memory());
        let content = WorkspaceContent::new(Arc::clone(&store), tokio::runtime::Handle::current());
        // Large enough to be file-backed, and a small one kept as loose blobs.
        for len in [3 * 1024 * 1024, 10_000] {
            let bytes: Vec<u8> = (0..len)
                .map(|index| u8::try_from(index % 251).unwrap())
                .collect();
            let manifest = *store
                .write_content(&bytes)
                .await
                .unwrap()
                .unwrap()
                .as_bytes();
            let digest = *blake3::hash(&bytes).as_bytes();
            assert_ne!(manifest, digest, "the two addresses differ");

            // Present in the store, but nobody bound it: unknown.
            let unbound = content.clone();
            assert_eq!(
                tokio::task::spawn_blocking(move || unbound.get(digest))
                    .await
                    .unwrap(),
                Ok(None)
            );
            assert_eq!(content.bind_file(manifest).await, Ok(digest));
            let bound = content.clone();
            assert_eq!(
                tokio::task::spawn_blocking(move || bound.get(digest))
                    .await
                    .unwrap(),
                Ok(Some(bytes))
            );
            assert_eq!(content.manifest_of(digest), Some(manifest));
        }
        assert_eq!(
            content.bind_file([7; 32]).await,
            Err(BindError::UnknownManifest)
        );

        // An output: checked against its digest, then a file version.
        let output = b"a thumbnail".to_vec();
        let digest = *blake3::hash(&output).as_bytes();
        let writer = content.clone();
        let wrong = output.clone();
        assert!(
            tokio::task::spawn_blocking(move || writer.put([1; 32], &wrong))
                .await
                .unwrap()
                .is_err()
        );
        let writer = content.clone();
        let stored = output.clone();
        tokio::task::spawn_blocking(move || writer.put(digest, &stored))
            .await
            .unwrap()
            .unwrap();
        let manifest = content.manifest_of(digest).unwrap();
        assert_eq!(read_version(Some(&*store), manifest).await.unwrap(), output);
    }

    #[tokio::test]
    async fn a_replica_holds_only_the_located_version_that_hashes_to_the_digest() {
        let root = tempfile::tempdir().unwrap();
        let replica = root.path().join("replica");
        // The replica process keeps its store open throughout.
        let store = ContentStore::open(&replica).await.unwrap();
        let bytes: Vec<u8> = (0..2 * 1024 * 1024_u32)
            .map(|index| u8::try_from(index % 241).unwrap())
            .collect();
        let digest = *blake3::hash(&bytes).as_bytes();
        let manifest = *store
            .write_content(&bytes)
            .await
            .unwrap()
            .unwrap()
            .as_bytes();
        let len = u64::try_from(bytes.len()).unwrap();
        let held = ReplicaContent::new(vec![root.path().join("absent"), replica]);
        assert!(held.locates());

        assert_eq!(held.held(digest, Some(manifest), len), Ok(Some(bytes)));
        // A digest names no file here: without the requester's locator,
        // nothing is held.
        assert_eq!(held.held(digest, None, u64::MAX), Ok(None));
        // The located file is these bytes only if they hash to the digest.
        assert_eq!(held.held([3; 32], Some(manifest), u64::MAX), Ok(None));
        assert_eq!(held.held(digest, Some([4; 32]), u64::MAX), Ok(None));
        assert_eq!(held.held(digest, Some(manifest), len - 1), Ok(None));
        drop(store);
    }

    /// Serve `store` as `fs replica` does, from `cache_dir`.
    fn serve_replica(
        cache_dir: &std::path::Path,
        store: Arc<ContentStore>,
    ) -> tokio::task::JoinHandle<()> {
        let socket = fs::replica_content::socket_path(cache_dir);
        let listener = tilo_files::native::private::bind_socket(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::UnixListener::from_std(listener).unwrap();
        tokio::spawn(fs::replica_content::serve(listener, store))
    }

    /// `held.held(..)` off the runtime, as staging calls it.
    async fn held_off_runtime(
        held: &ReplicaContent,
        digest: [u8; 32],
        locator: [u8; 32],
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        let held = held.clone();
        tokio::task::spawn_blocking(move || held.held(digest, Some(locator), max_bytes))
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replica_holds_its_small_versions_through_the_replica_itself() {
        let root = tempfile::tempdir().unwrap();
        let replica = root.path().join("replica");
        let store = Arc::new(ContentStore::open(&replica).await.unwrap());
        // Small enough to be loose blobs inside the store's database.
        let bytes: Vec<u8> = (0..200 * 1024_u32)
            .map(|index| u8::try_from(index % 241).unwrap())
            .collect();
        let digest = *blake3::hash(&bytes).as_bytes();
        let manifest = *store
            .write_content(&bytes)
            .await
            .unwrap()
            .unwrap()
            .as_bytes();
        let len = u64::try_from(bytes.len()).unwrap();
        let held = ReplicaContent::new(vec![replica.clone()]);
        // Not a file: unreachable until the replica answers for it.
        assert_eq!(
            held_off_runtime(&held, digest, manifest, len).await,
            Ok(None)
        );

        let server = serve_replica(&replica, Arc::clone(&store));
        assert_eq!(
            held_off_runtime(&held, digest, manifest, len).await,
            Ok(Some(bytes))
        );
        // Still only the located version that hashes to the digest.
        assert_eq!(
            held_off_runtime(&held, [3; 32], manifest, len).await,
            Ok(None)
        );
        assert_eq!(
            held_off_runtime(&held, digest, [4; 32], len).await,
            Ok(None)
        );
        assert_eq!(
            held_off_runtime(&held, digest, manifest, len - 1).await,
            Ok(None)
        );
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bytes_a_replica_misreports_are_not_held() {
        let root = tempfile::tempdir().unwrap();
        let replica = root.path().join("replica");
        std::fs::create_dir_all(&replica).unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(fs::replica_content::socket_path(&replica))
                .unwrap();
        // A replica answering every request with bytes that are not the
        // version asked for.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let framing = fs::replica_content::FRAMING;
                if let Ok(Some(_)) =
                    framing.read_value::<_, fs::replica_content::Request>(&mut stream)
                {
                    let wrong = fs::replica_content::Response::Found(b"other bytes".to_vec());
                    let _ = framing.write_value(&mut stream, &wrong);
                }
            }
        });
        let bytes = b"the requester's bytes".to_vec();
        let digest = *blake3::hash(&bytes).as_bytes();
        let held = ReplicaContent::new(vec![replica]);
        assert_eq!(
            held_off_runtime(&held, digest, [5; 32], u64::MAX).await,
            Ok(None),
            "fetched instead"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_bound_blobs_are_served_by_their_own_digest() {
        let store = Arc::new(ContentStore::memory());
        let content = WorkspaceContent::new(Arc::clone(&store), tokio::runtime::Handle::current());
        let media = DocumentMedia::new(Arc::clone(&store));
        let room = BlobScope::new("room", "so.tilo.photos").unwrap();
        let bytes = b"document media a room put here".to_vec();
        let digest = *media.store(&room, bytes.clone()).await.unwrap().as_bytes();
        assert_eq!(digest, *blake3::hash(&bytes).as_bytes(), "one address");
        assert_eq!(content.blob_len(&room, digest).await, Ok(30));

        // In the store, but nobody bound it: unknown.
        let unbound = content.clone();
        assert_eq!(
            tokio::task::spawn_blocking(move || unbound.get(digest))
                .await
                .unwrap(),
            Ok(None)
        );
        // Another room does not reach it, whatever the store holds.
        let elsewhere = BlobScope::new("another-room", "so.tilo.photos").unwrap();
        assert_eq!(
            content.blob_len(&elsewhere, digest).await,
            Err(BindError::NotLocal)
        );
        assert_eq!(
            content.bind_blob(&elsewhere, digest).await,
            Err(BindError::NotLocal)
        );
        let still_unbound = content.clone();
        assert_eq!(
            tokio::task::spawn_blocking(move || still_unbound.get(digest))
                .await
                .unwrap(),
            Ok(None)
        );
        assert_eq!(content.bind_blob(&room, digest).await, Ok(30));
        let bound = content.clone();
        assert_eq!(
            tokio::task::spawn_blocking(move || bound.get(digest))
                .await
                .unwrap(),
            Ok(Some(bytes))
        );
        assert_eq!(content.manifest_of(digest), None, "no file version");
        assert_eq!(
            content.bind_blob(&room, [9; 32]).await,
            Err(BindError::NotLocal)
        );

        // An output kept for the room is its media, and bound.
        let output = b"a thumbnail".to_vec();
        let made = *blake3::hash(&output).as_bytes();
        assert!(
            content
                .keep_blob(&room, [7; 32], output.clone())
                .await
                .is_err(),
            "bytes that are not their digest are refused"
        );
        content
            .keep_blob(&room, made, output.clone())
            .await
            .unwrap();
        assert!(media.contains(&room, made.into()).await, "the room's media");
        assert!(!media.contains(&elsewhere, made.into()).await);
        assert_eq!(content.read(made).await, Ok(Some(output)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_session_without_workspace_files_binds_only_its_rooms_media() {
        let store = Arc::new(ContentStore::memory());
        let media = DocumentMedia::new(Arc::clone(&store));
        let content = WorkspaceContent::of_media(media.clone(), tokio::runtime::Handle::current());
        let room = BlobScope::new("room", "so.tilo.photos").unwrap();
        let bytes = b"a photograph".to_vec();
        let digest = *media.store(&room, bytes.clone()).await.unwrap().as_bytes();
        let manifest = *store
            .write_content(&bytes)
            .await
            .unwrap()
            .unwrap()
            .as_bytes();
        assert_eq!(
            content.bind_file(manifest).await,
            Err(BindError::UnknownManifest),
            "no workspace files in this process"
        );
        assert!(content.store(digest, &bytes).await.is_err());
        assert_eq!(content.bind_blob(&room, digest).await, Ok(12));
        let held = content.clone();
        assert_eq!(
            tokio::task::spawn_blocking(move || held.held(digest, Some(manifest), u64::MAX))
                .await
                .unwrap(),
            Ok(None),
            "nothing is held for a worker"
        );
        let bound = content.clone();
        assert_eq!(
            tokio::task::spawn_blocking(move || bound.get(digest))
                .await
                .unwrap(),
            Ok(Some(bytes))
        );
    }
}
