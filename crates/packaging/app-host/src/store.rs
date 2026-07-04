//! On-disk content-addressed blob store + per-package eagerly-extracted
//! payload tree, owning the [`registry`](crate::registry) too.
//!
//! Layout:
//!
//! ```text
//! $root/
//!   store/<hex>              # one file per BLAKE3 hash, content-addressed
//!   payloads/<package-id>/   # eagerly-extracted payload tree
//!   registry.toml            # `Pinning` + `last_used_unix` index
//! ```
//!
//! `Store` is the synchronous façade the service uses. It wraps a
//! [`pkg::BlobStore`]-shaped `FsBlobStore` so we can hand the
//! same store to `pkg::unpack` without copying bytes.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use thiserror::Error;

use pkg::blobs::{BlobHash, BlobStore, BlobStoreError};
use pkg::id::PackageId;

use crate::registry::{Pinning, Registry, RegistryEntry, RegistryError};

/// File-system content-addressed blob store. Blobs live as
/// `$root/store/<hex>`. Idempotent — writing the same bytes twice
/// is a no-op.
pub struct FsBlobStore {
    root: PathBuf,
}

impl FsBlobStore {
    /// Create / open a store rooted at `root`. The `store/`
    /// subdirectory is created lazily on first `put`.
    pub fn new(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn blob_path(&self, hash: &BlobHash) -> PathBuf {
        self.root.join(hash.to_hex())
    }

    /// Sum the byte length of every blob in the store. Linear in the
    /// number of blobs; intended for v0 — a future cache could keep a
    /// running total.
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

    /// Remove a blob from disk. Returns `Ok(bytes_freed)` — `0` when
    /// the blob is absent (idempotent). Used by the post-remove GC to
    /// reclaim disk from blobs no longer referenced by any package.
    pub fn delete(&mut self, hash: &BlobHash) -> io::Result<u64> {
        let path = self.blob_path(hash);
        match fs::metadata(&path) {
            Ok(md) if md.is_file() => {
                let len = md.len();
                fs::remove_file(&path)?;
                Ok(len)
            }
            Ok(_) => Ok(0),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e),
        }
    }
}

impl BlobStore for FsBlobStore {
    fn put(&mut self, bytes: &[u8]) -> Result<BlobHash, BlobStoreError> {
        let h = BlobHash::of(bytes);
        let path = self.blob_path(&h);
        if !path.exists() {
            // Write to a tempfile + rename so the visible blob is
            // either absent or complete; concurrent writers of the
            // same bytes converge on the same final file.
            let tmp = path.with_extension("tmp");
            fs::write(&tmp, bytes).map_err(BlobStoreError::Io)?;
            // rename is atomic on a single filesystem; if another
            // process raced ahead and produced the same blob bytes,
            // the destination is now correct either way.
            let _ = fs::rename(&tmp, &path);
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

/// Top-level Store: blob store + payload tree manager + registry.
/// All access goes through `Mutex<Inner>` so the service can hand a
/// single `Arc<Store>` to its capnp tasks.
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
}

impl Store {
    /// Open / create a store rooted at `root`. Creates every
    /// required subdirectory.
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
    pub fn resolve(&self, id: PackageId) -> Result<RegistryEntry, StoreError> {
        let inner = self.inner.lock();
        inner
            .registry
            .get(&id)
            .cloned()
            .ok_or(StoreError::Unknown(id))
    }

    /// Flip Tried → Pinned (idempotent).
    pub fn pin(&self, id: PackageId) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        if !inner.registry.set_pinning(&id, Pinning::Pinned) {
            return Err(StoreError::Unknown(id));
        }
        inner.persist_registry()?;
        Ok(())
    }

    /// Flip Pinned → Tried (idempotent).
    pub fn unpin(&self, id: PackageId) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        if !inner.registry.set_pinning(&id, Pinning::Tried) {
            return Err(StoreError::Unknown(id));
        }
        inner.persist_registry()?;
        Ok(())
    }

    /// LRU touch.
    pub fn mark_used(&self, id: PackageId) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        if !inner.registry.touch(&id) {
            return Err(StoreError::Unknown(id));
        }
        inner.persist_registry()?;
        Ok(())
    }

    /// Remove a Tried package: drop the registry entry, the payload
    /// tree, and any blobs unique to this package. Refuses to remove
    /// Pinned packages.
    pub fn remove(&self, id: PackageId) -> Result<u64, StoreError> {
        let mut inner = self.inner.lock();
        let Some(entry) = inner.registry.get(&id).cloned() else {
            return Err(StoreError::Unknown(id));
        };
        if entry.pinning == Pinning::Pinned {
            return Err(StoreError::PinnedRemove(id));
        }
        // Drop the entry from the registry first so blob-sharing
        // checks reflect the new state.
        inner.registry.remove(&id);
        inner.persist_registry()?;
        let freed = inner.gc_after_remove(&id, &entry)?;
        Ok(freed)
    }

    /// Insert an entry directly. Used by the `install` flow after
    /// the qpkg has been parsed + the payload extracted.
    pub fn put_entry(&self, entry: RegistryEntry) -> Result<(), StoreError> {
        let mut inner = self.inner.lock();
        inner.registry.insert(entry);
        inner.persist_registry()?;
        Ok(())
    }

    /// Lock + hand out the inner blob store for an `install`
    /// operation that needs to ingest several blobs and then unpack
    /// them via `pkg::unpack`.
    pub fn with_blobs<R>(&self, f: impl FnOnce(&mut FsBlobStore) -> R) -> R {
        let mut inner = self.inner.lock();
        f(&mut inner.blobs)
    }

    /// `(total_bytes, by_pinning)` reported from on-disk reality:
    /// blob-store occupancy + payload-tree occupancy.
    pub fn disk_usage(&self) -> Result<DiskUsage, StoreError> {
        let inner = self.inner.lock();
        let payloads_root = inner.root.join("payloads");
        let store_total = inner.blobs.total_bytes()?;
        let mut by_pinning = BTreeMap::<Pinning, u64>::new();
        for entry in inner.registry.entries() {
            let pl = payloads_root.join(entry.id.to_hex());
            let size = dir_size(&pl).unwrap_or(0);
            *by_pinning.entry(entry.pinning).or_default() += size;
        }
        let pinned_bytes = by_pinning.get(&Pinning::Pinned).copied().unwrap_or(0);
        let tried_bytes = by_pinning.get(&Pinning::Tried).copied().unwrap_or(0);
        // The blob-store is content-addressed so blobs are shared
        // across packages — we report total = store + payloads to
        // surface both axes (caller can decide which it cares about).
        Ok(DiskUsage {
            total_bytes: store_total
                .saturating_add(pinned_bytes)
                .saturating_add(tried_bytes),
            pinned_bytes,
            tried_bytes,
            store_bytes: store_total,
        })
    }

    /// Total store-only bytes (blob-store + every payload tree).
    /// Used by the GC heuristic.
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

    /// Remove the payload tree, then sweep blobs that are no longer
    /// referenced by any remaining entry. Returns the bytes freed
    /// from the payload tree PLUS the reclaimed content-addressed
    /// blobs.
    ///
    /// Blob-reclamation logic: build the removed entry's blob set
    /// (its signed-root, manifest, collection, and every file in the
    /// collection) and subtract the union of every remaining entry's
    /// blob set. Blobs left over are unreferenced and get deleted from
    /// the content-addressed store. Errors during blob discovery for
    /// remaining entries are logged and cause that entry's contribution
    /// to be treated as "everything still live" — we bias toward
    /// keeping data over aggressive collection.
    fn gc_after_remove(&mut self, id: &PackageId, _entry: &RegistryEntry) -> Result<u64, StoreError> {
        let payload_dir = self.root.join("payloads").join(id.to_hex());
        let payload_freed = dir_size(&payload_dir).unwrap_or(0);
        if payload_dir.exists() {
            fs::remove_dir_all(&payload_dir)?;
        }

        // Content-addressed blob layer: figure out which blobs the
        // removed entry pulled in, then subtract every blob still
        // referenced by a surviving entry.
        let removed_set = blob_set_for(&self.blobs, id).unwrap_or_default();
        let mut still_live: std::collections::BTreeSet<BlobHash> =
            std::collections::BTreeSet::new();
        for entry in self.registry.entries() {
            match blob_set_for(&self.blobs, &entry.id) {
                Ok(set) => still_live.extend(set),
                Err(e) => {
                    // Refuse to over-collect: without a full picture of
                    // this entry's blobs, we can't safely reclaim
                    // anything they might reference. Log and mark
                    // every hash the removed set contains as still
                    // live so the sweep is a no-op for this call.
                    tracing::warn!(
                        entry_id = %entry.id,
                        error = ?e,
                        "gc: could not enumerate blob set for surviving entry; skipping blob reclamation to avoid over-collection"
                    );
                    still_live.extend(removed_set.iter().copied());
                    break;
                }
            }
        }

        let mut blob_freed = 0u64;
        for hash in &removed_set {
            if still_live.contains(hash) {
                continue;
            }
            match self.blobs.delete(hash) {
                Ok(bytes) => blob_freed = blob_freed.saturating_add(bytes),
                Err(e) => {
                    tracing::warn!(
                        blob = %hash,
                        error = %e,
                        "gc: failed to delete unreferenced blob"
                    );
                }
            }
        }

        Ok(payload_freed.saturating_add(blob_freed))
    }
}

/// Reconstruct the set of blobs a package references: its signed-root
/// (== the PackageId), the manifest blob, the collection blob, and
/// every payload-file blob listed in the collection. Reads the store
/// directly rather than the payload tree so a corrupted payload tree
/// doesn't sabotage GC accounting.
fn blob_set_for(
    blobs: &FsBlobStore,
    id: &PackageId,
) -> Result<std::collections::BTreeSet<BlobHash>, StoreError> {
    let mut set = std::collections::BTreeSet::new();
    let root_hash = BlobHash(*id.as_bytes());
    set.insert(root_hash);
    let Some(signed_bytes) = blobs.get(&root_hash)? else {
        return Ok(set);
    };
    let signed = pkg::signed::SignedRoot::decode(&signed_bytes).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("signed-root decode: {e}"))
    })?;
    set.insert(signed.manifest_hash);
    set.insert(signed.payload_root_hash);
    if let Some(collection_bytes) = blobs.get(&signed.payload_root_hash)? {
        let collection = pkg::blobs::Collection::decode(&collection_bytes).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("collection decode: {e}"))
        })?;
        for hash in collection.entries.values() {
            set.insert(*hash);
        }
    }
    Ok(set)
}

/// Recursive byte sum of every regular file under `path`. Returns
/// `0` if the dir is absent.
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
