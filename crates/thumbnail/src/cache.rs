//! A content-addressed thumbnail cache: disk (survives restart) + memory LRU.
//!
//! The key is `(content hash, target edge)` — the same BLAKE3 the rest of qOS
//! addresses content by. Because it is keyed by *content*, a thumbnail is
//! invalidated for free when the file changes (the file's hash changes, so the
//! key changes), and shared for free when the same bytes appear under two paths
//! or on two peers. Stored as PNG on disk under the cache directory, fronted by
//! a bounded `moka` LRU so a hot grid never re-reads the disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use moka::sync::Cache;

/// A cache key: the content's BLAKE3 hash plus the longest-edge target the
/// thumbnail was generated for (so the same file cached at two sizes coexists).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    pub content: [u8; 32],
    pub edge: u32,
}

impl ThumbKey {
    /// Key a thumbnail by the BLAKE3 hash of its source bytes at `edge`.
    pub fn of_bytes(bytes: &[u8], edge: u32) -> Self {
        Self {
            content: *blake3::hash(bytes).as_bytes(),
            edge,
        }
    }

    /// Key a thumbnail by an already-known content hash (e.g. the FUSE
    /// filesystem's manifest hash) at `edge`. This is the fully
    /// content-addressed path — a thumbnail is shared across paths and peers.
    pub fn of_hash(content: [u8; 32], edge: u32) -> Self {
        Self { content, edge }
    }

    /// A cheap identity key for a local file that has no precomputed content
    /// hash: BLAKE3 over `path + mtime + size`. Hashing the full bytes on the
    /// request thread would be the very jank the off-thread service exists to
    /// avoid; a `(path, mtime, size)` tuple still changes whenever the file's
    /// content changes, so the cache invalidates correctly on edit. Collab-FS
    /// files should use [`ThumbKey::of_hash`] instead for true content dedup.
    pub fn of_path_meta(path: &Path, mtime_ms: u64, size: u64, edge: u32) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(path.as_os_str().as_encoded_bytes());
        hasher.update(&mtime_ms.to_le_bytes());
        hasher.update(&size.to_le_bytes());
        Self {
            content: *hasher.finalize().as_bytes(),
            edge,
        }
    }

    /// The on-disk filename: `<hex>-<edge>.png`.
    fn filename(&self) -> String {
        format!("{}-{}.png", hex::encode(self.content), self.edge)
    }
}

/// Content-addressed PNG-thumbnail store.
#[derive(Clone)]
pub struct ThumbCache {
    dir: PathBuf,
    mem: Cache<ThumbKey, Arc<Vec<u8>>>,
    /// Cap on the number of PNGs kept on disk; the least-recently-modified are
    /// pruned past it so the cache stays bounded across sessions.
    max_disk_entries: usize,
}

impl ThumbCache {
    /// Open (creating if needed) a cache rooted at `dir` with default bounds.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        Self::with_limits(dir, 512, 4096)
    }

    /// Open with explicit memory (`mem_capacity` entries) and disk
    /// (`max_disk_entries` files) bounds.
    pub fn with_limits(
        dir: impl AsRef<Path>,
        mem_capacity: u64,
        max_disk_entries: usize,
    ) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            mem: Cache::new(mem_capacity),
            max_disk_entries,
        })
    }

    fn path_for(&self, key: &ThumbKey) -> PathBuf {
        self.dir.join(key.filename())
    }

    /// The cached PNG bytes for `key`: memory first, then disk (promoting a disk
    /// hit into memory), else `None`.
    pub fn get(&self, key: &ThumbKey) -> Option<Arc<Vec<u8>>> {
        if let Some(hit) = self.mem.get(key) {
            return Some(hit);
        }
        let bytes = std::fs::read(self.path_for(key)).ok()?;
        if bytes.is_empty() {
            return None; // an empty file is a torn write — treat as a miss
        }
        let arc = Arc::new(bytes);
        self.mem.insert(*key, arc.clone());
        Some(arc)
    }

    /// Whether `key` is present in memory or on disk.
    pub fn contains(&self, key: &ThumbKey) -> bool {
        self.mem.contains_key(key) || self.path_for(key).exists()
    }

    /// Store `png` for `key` in memory and on disk (atomic temp-then-rename so a
    /// crash mid-write never leaves a partial thumbnail). Prunes the disk cache
    /// back under its bound afterward.
    pub fn put(&self, key: &ThumbKey, png: Vec<u8>) {
        let arc = Arc::new(png);
        self.mem.insert(*key, arc.clone());
        let final_path = self.path_for(key);
        // temp path carries the pid+key so concurrent writers don't collide.
        let tmp = self
            .dir
            .join(format!(".{}.{}.tmp", std::process::id(), key.filename()));
        if std::fs::write(&tmp, arc.as_slice()).is_ok() {
            let _ = std::fs::rename(&tmp, &final_path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        self.prune();
    }

    /// If the disk holds more than `max_disk_entries` thumbnails, delete the
    /// least-recently-modified until it is back at ~90% of the cap.
    fn prune(&self) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut entries: Vec<(std::time::SystemTime, PathBuf)> = rd
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
            .filter_map(|e| {
                let path = e.path();
                let mtime = e.metadata().ok()?.modified().ok()?;
                Some((mtime, path))
            })
            .collect();
        if entries.len() <= self.max_disk_entries {
            return;
        }
        // Oldest first, remove down to 90% of the cap.
        entries.sort_by_key(|(t, _)| *t);
        let target = (self.max_disk_entries * 9) / 10;
        let to_remove = entries.len().saturating_sub(target);
        for (_, path) in entries.into_iter().take(to_remove) {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn key_is_content_addressed() {
        let a = ThumbKey::of_bytes(b"hello", 256);
        let b = ThumbKey::of_bytes(b"hello", 256);
        let c = ThumbKey::of_bytes(b"world", 256);
        let d = ThumbKey::of_bytes(b"hello", 128);
        assert_eq!(a, b, "same bytes + size → same key");
        assert_ne!(a, c, "different bytes → different key");
        assert_ne!(a, d, "different size → different key");
    }

    #[test]
    fn put_then_get_hits() {
        let dir = tmpdir();
        let cache = ThumbCache::open(dir.path()).unwrap();
        let key = ThumbKey::of_bytes(b"file-content", 256);
        assert!(cache.get(&key).is_none(), "cold miss");
        cache.put(&key, vec![1, 2, 3, 4]);
        assert_eq!(
            cache.get(&key).unwrap().as_slice(),
            &[1, 2, 3, 4],
            "warm hit"
        );
        assert!(cache.contains(&key));
    }

    #[test]
    fn survives_a_reopen() {
        let dir = tmpdir();
        let key = ThumbKey::of_bytes(b"persist-me", 256);
        {
            let cache = ThumbCache::open(dir.path()).unwrap();
            cache.put(&key, vec![9, 8, 7]);
        }
        // A brand-new cache instance on the same directory finds it on disk —
        // no in-memory state carried over.
        let reopened = ThumbCache::open(dir.path()).unwrap();
        assert_eq!(
            reopened.get(&key).unwrap().as_slice(),
            &[9, 8, 7],
            "thumbnail survives a restart via the on-disk cache"
        );
    }

    #[test]
    fn disk_cache_stays_bounded() {
        let dir = tmpdir();
        // cap of 10 files → after inserting 40 the pruner keeps it near 90% (9).
        let cache = ThumbCache::with_limits(dir.path(), 64, 10).unwrap();
        for i in 0..40u32 {
            cache.put(
                &ThumbKey::of_bytes(format!("f{i}").as_bytes(), 256),
                vec![i as u8; 8],
            );
        }
        let count = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
            .count();
        assert!(count <= 10, "disk cache pruned to its bound, got {count}");
    }
}
