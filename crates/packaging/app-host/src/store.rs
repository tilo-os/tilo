//! On-disk content-addressed blob store + per-package eagerly-extracted
//! payload tree, owning the [`registry`](crate::registry) too.
//!
//! ```text
//! $root/
//!   store/<hex>              # one file per BLAKE3 hash, content-addressed
//!   payloads/<package-id>/   # eagerly-extracted payload tree
//!   registry.toml            # `Pinning` + `last_used_unix` index
//! ```
//!
//! `Store` is the synchronous façade the service uses; it wraps a
//! [`pkg::BlobStore`]-shaped `FsBlobStore` so the same store can go to
//! `pkg::unpack` without copying bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use thiserror::Error;

use pkg::blobs::{BlobHash, BlobStore, BlobStoreError, Collection};
use pkg::id::PackageId;
use pkg::signed::SignedRoot;

use crate::registry::{Pinning, Registry, RegistryEntry, RegistryError};

/// File-system content-addressed blob store; blobs live as
/// `$root/store/<hex>`. Idempotent — writing the same bytes twice is a no-op.
pub struct FsBlobStore {
    root: PathBuf,
}

impl FsBlobStore {
    /// Create / open a store rooted at `root`; the `store/` subdirectory is created lazily on first `put`.
    ///
    /// # Errors
    ///
    /// Returns an error if `root` cannot be created (permission denied, or a
    /// non-directory already occupies the path).
    pub fn new(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn blob_path(&self, hash: &BlobHash) -> PathBuf {
        self.root.join(hash.to_hex())
    }

    /// Sum the byte length of every blob in the store. Linear in blob
    /// count; fine for v0 — a future cache could keep a running total.
    ///
    /// # Errors
    ///
    /// Returns an error if the store directory cannot be read, or if
    /// reading an entry's metadata fails.
    pub fn total_bytes(&self) -> io::Result<u64> {
        let mut total = 0u64;
        if !self.root.exists() {
            return Ok(0);
        }
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let md = entry.metadata()?;
            if md.is_file() {
                total = total.saturating_add(md.len());
            }
        }
        Ok(total)
    }
}

impl BlobStore for FsBlobStore {
    fn put(&mut self, bytes: &[u8]) -> Result<BlobHash, BlobStoreError> {
        let h = BlobHash::of(bytes);
        let path = self.blob_path(&h);
        if !path.exists() {
            // The visible blob is either absent or complete. If another process
            // raced ahead and wrote the same content-addressed bytes, the
            // destination is correct either way — but a genuine IO failure is still reported.
            atomic_file::write(&path, bytes).map_err(BlobStoreError::Io)?;
        }
        Ok(h)
    }

    fn get(&self, hash: &BlobHash) -> Result<Option<Vec<u8>>, BlobStoreError> {
        let path = self.blob_path(hash);
        match fs::read(&path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(BlobStoreError::Io(e)),
        }
    }

    fn has(&self, hash: &BlobHash) -> bool {
        self.blob_path(hash).exists()
    }
}

/// Top-level Store: blob store + payload tree manager + registry, all
/// behind `Mutex<Inner>` so the service can hand a single `Arc<Store>`
/// to its capnp tasks.
pub struct Store {
    inner: Mutex<Inner>,
}

struct Inner {
    root: PathBuf,
    blobs: FsBlobStore,
    registry: Registry,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("registry: {0}")]
    Registry(#[from] RegistryError),
    #[error("blob-store: {0}")]
    Blob(#[from] BlobStoreError),
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("unknown package id {0}")]
    Unknown(PackageId),
    #[error("refusing to remove pinned package {0}")]
    PinnedRemove(PackageId),
    #[error("installed package {0} has an unreadable content graph: {1}")]
    CorruptLivePackage(PackageId, String),
}

impl Store {
    /// Open / create a store rooted at `root`, creating every required subdirectory.
    ///
    /// # Errors
    ///
    /// Returns an error if the `store/` or `payloads/` subdirectories
    /// cannot be created, if the on-disk registry cannot be loaded, or if
    /// the blob store cannot be opened.
    pub fn open(root: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(root.join("store"))?;
        fs::create_dir_all(root.join("payloads"))?;
        let registry = Registry::load(&root.join("registry.toml"))?;
        let blobs = FsBlobStore::new(root.join("store"))?;
        Ok(Self {
            inner: Mutex::new(Inner {
                root: root.to_path_buf(),
                blobs,
                registry,
            }),
        })
    }

    /// Root directory the store occupies. Useful for tests.
    pub fn root(&self) -> PathBuf {
        self.inner.lock().root.clone()
    }

    /// Sub-directory where extracted payload trees live.
    pub fn payloads_root(&self) -> PathBuf {
        self.inner.lock().root.join("payloads")
    }

    /// All currently-known registry entries.
    pub fn list(&self) -> Vec<RegistryEntry> {
        self.inner.lock().registry.entries().cloned().collect()
    }

    /// Resolve a single id.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Unknown`] if `id` is not present in the registry.
    pub fn resolve(&self, id: PackageId) -> Result<RegistryEntry, StoreError> {
        let inner = self.inner.lock();
        inner
            .registry
            .get(&id)
            .cloned()
            .ok_or(StoreError::Unknown(id))
    }

    /// Flip Tried → Pinned (idempotent).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Unknown`] if `id` is not present in the
    /// registry, or an error if persisting the updated registry fails.
    pub fn pin(&self, id: PackageId) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        if !inner.registry.set_pinning(&id, Pinning::Pinned) {
            return Err(StoreError::Unknown(id));
        }
        inner.persist_registry()?;
        drop(inner);
        Ok(())
    }

    /// Flip Pinned → Tried (idempotent).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Unknown`] if `id` is not present in the
    /// registry, or an error if persisting the updated registry fails.
    pub fn unpin(&self, id: PackageId) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        if !inner.registry.set_pinning(&id, Pinning::Tried) {
            return Err(StoreError::Unknown(id));
        }
        inner.persist_registry()?;
        drop(inner);
        Ok(())
    }

    /// LRU touch.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Unknown`] if `id` is not present in the
    /// registry, or an error if persisting the updated registry fails.
    pub fn mark_used(&self, id: PackageId) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        if !inner.registry.touch(&id) {
            return Err(StoreError::Unknown(id));
        }
        inner.persist_registry()?;
        drop(inner);
        Ok(())
    }

    /// Remove a Tried package: drop the registry entry, the payload tree,
    /// and every content-addressed blob no remaining package references.
    /// Refuses to remove Pinned packages.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Unknown`] if `id` is not present in the
    /// registry, [`StoreError::PinnedRemove`] if the package is currently
    /// pinned, or an error if persisting the registry or removing the
    /// payload tree fails.
    pub fn remove(&self, id: PackageId) -> Result<u64, StoreError> {
        let mut inner = self.inner.lock();
        let Some(entry) = inner.registry.get(&id).cloned() else {
            return Err(StoreError::Unknown(id));
        };
        if entry.pinning == Pinning::Pinned {
            return Err(StoreError::PinnedRemove(id));
        }
        // Drop the entry from the registry first so blob-sharing checks reflect the new state.
        inner.registry.remove(&id);
        inner.persist_registry()?;
        let freed = inner.gc_after_remove(&id, &entry)?;
        drop(inner);
        Ok(freed)
    }

    /// Insert an entry directly; used by the install flow after the qpkg has been parsed + the payload extracted.
    ///
    /// # Errors
    ///
    /// Returns an error if persisting the updated registry fails.
    pub fn put_entry(&self, entry: RegistryEntry) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        inner.registry.insert(entry);
        inner.persist_registry()?;
        drop(inner);
        Ok(())
    }

    /// Lock + hand out the inner blob store for an install operation that
    /// needs to ingest several blobs and then unpack via `pkg::unpack`.
    pub fn with_blobs<R>(&self, f: impl FnOnce(&mut FsBlobStore) -> R) -> R {
        let mut inner = self.inner.lock();
        f(&mut inner.blobs)
    }

    /// (`total_bytes`, `by_pinning`) reported from on-disk reality: blob-store occupancy + payload-tree occupancy.
    ///
    /// # Errors
    ///
    /// Returns an error if the blob store's total size cannot be read.
    pub fn disk_usage(&self) -> Result<DiskUsage, StoreError> {
        let inner = self.inner.lock();
        let payloads_root = inner.root.join("payloads");
        let store_total = inner.blobs.total_bytes()?;
        let mut by_pinning = BTreeMap::<Pinning, u64>::new();
        for entry in inner.registry.entries() {
            let pl = payloads_root.join(entry.id.to_hex());
            let size = dir_size(&pl).unwrap_or(0);
            let counter = by_pinning.entry(entry.pinning).or_default();
            *counter = counter.saturating_add(size);
        }
        drop(inner);
        let pinned_bytes = by_pinning.get(&Pinning::Pinned).copied().unwrap_or(0);
        let tried_bytes = by_pinning.get(&Pinning::Tried).copied().unwrap_or(0);
        // Blobs are content-addressed and shared across packages, so we
        // report total = store + payloads, surfacing both axes.
        Ok(DiskUsage {
            total_bytes: store_total
                .saturating_add(pinned_bytes)
                .saturating_add(tried_bytes),
            pinned_bytes,
            tried_bytes,
            store_bytes: store_total,
        })
    }

    /// Total store-only bytes (blob-store + every payload tree); used by the GC heuristic.
    ///
    /// # Errors
    ///
    /// Returns an error if [`Store::disk_usage`] fails.
    pub fn total_bytes_for_gc(&self) -> Result<u64, StoreError> {
        let u = self.disk_usage()?;
        Ok(u.total_bytes)
    }

    /// Read-only access to the registry. Used by GC.
    pub fn snapshot_registry(&self) -> Vec<RegistryEntry> {
        self.inner.lock().registry.entries().cloned().collect()
    }
}

/// On-disk reality reported to callers + the GC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskUsage {
    pub total_bytes: u64,
    pub pinned_bytes: u64,
    pub tried_bytes: u64,
    pub store_bytes: u64,
}

impl Inner {
    fn persist_registry(&self) -> Result<(), RegistryError> {
        self.registry.save(&self.root.join("registry.toml"))
    }

    /// Remove the payload tree, then derive the complete live content graph
    /// from the remaining registry and sweep unreferenced blobs. The store
    /// mutex is held throughout, so an install cannot race the mark phase.
    fn gc_after_remove(&self, id: &PackageId, _entry: &RegistryEntry) -> Result<u64, StoreError> {
        let payload_dir = self.root.join("payloads").join(id.to_hex());
        let mut freed = dir_size(&payload_dir).unwrap_or(0);
        if payload_dir.exists() {
            fs::remove_dir_all(&payload_dir)?;
        }
        let live = self.live_blob_names()?;
        for entry in fs::read_dir(&self.blobs.root)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if !metadata.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.len() != 64
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || live.contains(&name)
            {
                continue;
            }
            let bytes = metadata.len();
            fs::remove_file(entry.path())?;
            freed = freed.saturating_add(bytes);
        }
        Ok(freed)
    }

    /// Return the exact signed-root → manifest/collection → payload closure
    /// for every installed package. Any corrupt live root fails closed before
    /// the sweep removes a single blob.
    fn live_blob_names(&self) -> Result<BTreeSet<String>, StoreError> {
        let mut live = BTreeSet::new();
        for entry in self.registry.entries() {
            let root_hash = BlobHash(*entry.id.as_bytes());
            let root_bytes = self.blobs.get(&root_hash)?.ok_or_else(|| {
                StoreError::CorruptLivePackage(entry.id, "signed root is missing".into())
            })?;
            let root = SignedRoot::decode(&root_bytes)
                .map_err(|error| StoreError::CorruptLivePackage(entry.id, error.to_string()))?;
            let collection_bytes = self.blobs.get(&root.payload_root_hash)?.ok_or_else(|| {
                StoreError::CorruptLivePackage(entry.id, "payload collection is missing".into())
            })?;
            let collection = Collection::decode(&collection_bytes)
                .map_err(|error| StoreError::CorruptLivePackage(entry.id, error.to_string()))?;

            live.insert(root_hash.to_hex());
            live.insert(root.manifest_hash.to_hex());
            live.insert(root.payload_root_hash.to_hex());
            live.extend(collection.entries.values().map(BlobHash::to_hex));
        }
        Ok(live)
    }
}

/// Recursive byte sum of every regular file under `path`; returns 0 if the dir is absent.
pub(crate) fn dir_size(path: &Path) -> io::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        for entry in fs::read_dir(&p)? {
            let entry = entry?;
            let md = entry.metadata()?;
            if md.is_file() {
                total = total.saturating_add(md.len());
            } else if md.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    Ok(total)
}
