//! Size-pressure garbage collection.
//!
//! Policy: when `Store::total_bytes_for_gc()` exceeds the daemon's
//! `max_store_bytes` cap, evict `Tried` entries in LRU order (oldest
//! `last_used_unix` first) until the total drops below the cap.
//! `Pinned` entries are never touched — even if honouring that means
//! we can't reach the cap.
//!
//! The heuristic is intentionally simple:
//!
//! - We take a snapshot of the registry.
//! - Sort `Tried` entries by `last_used_unix` ascending.
//! - Call `Store::remove` on each one; that drops the registry row +
//!   the payload tree.
//! - Stop as soon as `total_bytes_for_gc` is under the cap.
//!
//! A cap of `0` disables the sweep (return an empty report). Callers
//! wire this into the daemon startup path and into `install` post-hooks
//! so a large install can't push the store over its budget without a
//! prompt cleanup pass.

use tilo_app_package::id::PackageId;

use crate::registry::{Pinning, RegistryEntry};
use crate::store::{Store, StoreError};

/// What the sweep did. `removed` lists the ids we dropped (order =
/// LRU-ascending); `freed_bytes` is the sum of the per-entry payload
/// sizes reported by `Store::remove`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcReport {
    pub removed: Vec<PackageId>,
    pub freed_bytes: u64,
}

impl GcReport {
    /// The empty report — nothing removed. Returned when the cap is
    /// `0` (GC disabled) or already under the cap.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Was anything removed?
    pub fn touched(&self) -> bool {
        !self.removed.is_empty()
    }
}

/// Run one size-pressure sweep. Removes Tried entries in LRU order
/// until the store is under `max_bytes` or no Tried entries remain.
///
/// - `max_bytes == 0` → GC is disabled; returns `GcReport::empty()`.
/// - The store's `Pinned` entries are never touched. If Pinned bytes
///   alone exceed `max_bytes` we return whatever we already freed.
///
/// # Errors
///
/// Returns an error if reading the store's total size or a candidate's
/// registry state fails, or if removing a candidate fails for a reason
/// other than it already being pinned or already gone.
pub fn sweep(store: &Store, max_bytes: u64) -> Result<GcReport, StoreError> {
    if max_bytes == 0 {
        return Ok(GcReport::empty());
    }
    let current = store.total_bytes_for_gc()?;
    if current <= max_bytes {
        return Ok(GcReport::empty());
    }

    // Snapshot the registry; sort Tried by last_used ascending.
    let mut candidates: Vec<RegistryEntry> = store
        .snapshot_registry()
        .into_iter()
        .filter(|e| e.pinning == Pinning::Tried)
        .collect();
    candidates.sort_by_key(|e| e.last_used_unix);

    let mut report = GcReport::empty();
    for c in candidates {
        // Bail out as soon as we're under-cap. Re-check here (rather
        // than trusting `current - freed`) because payload trees on
        // disk can grow larger than the manifest's declared size and
        // `Store::remove` walks the real fs tree.
        if store.total_bytes_for_gc()? <= max_bytes {
            break;
        }
        match store.remove(c.id) {
            Ok(freed) => {
                report.removed.push(c.id);
                report.freed_bytes = report.freed_bytes.saturating_add(freed);
            }
            // If a pinned pin-flip raced us the entry may already be
            // Pinned by the time we try to evict it. Just skip and
            // move on to the next candidate — do NOT surface this as
            // an error.
            Err(StoreError::PinnedRemove(_) | StoreError::Unknown(_)) => {}
            Err(other) => return Err(other),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::RegistryEntry;
    use std::fs;
    use tilo_app_package::blobs::{BlobStore, Collection};
    use tilo_app_package::id::PackageId;
    use tilo_app_package::signed::SignedRoot;

    fn write_payload(root: &std::path::Path, id: PackageId, bytes: usize) {
        let dir = root.join("payloads").join(id.to_hex());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("data.bin"), vec![0u8; bytes]).unwrap();
    }

    fn install_graph(store: &Store, seed: u8, pinning: Pinning, last_used_unix: u64) -> PackageId {
        let id = store.with_blobs(|blobs| {
            let payload_hash = blobs.put(&vec![seed; 1024]).unwrap();
            let collection = Collection::from_entries([("data.bin".into(), payload_hash)]);
            let collection_hash = blobs.put(&collection.encode()).unwrap();
            let manifest_hash = blobs.put(format!("manifest-{seed}").as_bytes()).unwrap();
            let root = SignedRoot::unsigned(manifest_hash, collection_hash, [seed; 32]);
            PackageId::from_bytes(blobs.put(&root.encode()).unwrap().0)
        });
        write_payload(&store.root(), id, 1024);
        let mut entry = RegistryEntry::new_now(
            id,
            pinning,
            store.payloads_root().join(id.to_hex()),
            String::new(),
        );
        entry.last_used_unix = last_used_unix;
        store.put_entry(entry).unwrap();
        id
    }

    #[test]
    fn disabled_when_cap_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let r = sweep(&store, 0).unwrap();
        assert!(r.removed.is_empty());
        assert_eq!(r.freed_bytes, 0);
    }

    #[test]
    fn under_cap_short_circuits() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let r = sweep(&store, u64::MAX).unwrap();
        assert!(!r.touched());
    }

    #[test]
    fn evicts_lru_tried_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        // 3 packages of 1KB each; 2 tried (t=1, t=2), one pinned.
        let a = install_graph(&store, 1, Pinning::Tried, 1);
        let _newer_tried = install_graph(&store, 2, Pinning::Tried, 2);
        let p = install_graph(&store, 3, Pinning::Pinned, 3);

        // Cap at 1 KiB — only Pinned should survive.
        let r = sweep(&store, 1024).unwrap();
        assert!(r.removed.contains(&a), "oldest Tried should evict first");
        // b may or may not be evicted depending on when we hit the
        // cap; pinned MUST survive.
        assert!(!r.removed.contains(&p), "Pinned must be preserved");

        let remaining: Vec<_> = store.list().into_iter().map(|e| e.id).collect();
        assert!(remaining.contains(&p));
        assert!(!remaining.contains(&a));
    }
}
