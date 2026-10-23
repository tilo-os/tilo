//! The owner's immutable history uses the existing durable content backend.
use std::{
    io::Read as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tilo_content::{ContentBackend, DirBackend};

/// Native document snapshots share this backend with smaller receipt/index blocks.
const MAX_HISTORY_BLOCK: usize = 3 * 1024 * 1024;

/// Durable content custody shared by owner stores using the same state directory.
/// This is not an authority: an admitted signed room root authenticates references.
pub struct HistoryContent {
    backend: Arc<dyn ContentBackend>,
    root: PathBuf,
    /// Whether the root and its entry in its parent have been flushed since
    /// this store was opened.
    root_durable: AtomicBool,
}
impl HistoryContent {
    /// Use the existing content implementation beside the actual state file.
    #[must_use]
    pub fn for_state_file(path: &Path) -> Self {
        let root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("content");
        Self {
            backend: Arc::new(DirBackend::new(root.clone())),
            root,
            root_durable: AtomicBool::new(false),
        }
    }
    /// Retain a block durably, before publishing its root.
    ///
    /// # Errors
    /// Quota, invalid block size or durable content I/O failure.
    pub fn put(&self, bytes: &[u8]) -> Result<[u8; 32], String> {
        if bytes.len() > MAX_HISTORY_BLOCK {
            return Err("native history block capacity exceeded".into());
        }
        let hash = *blake3::hash(bytes).as_bytes();
        let address = blake3::Hash::from_bytes(hash).to_hex().to_string();
        let shard = self
            .root
            .join(address.get(..2).ok_or("invalid content address")?);
        let path = shard.join(&address);
        let present = path.is_file();
        // The durable backend flushes a block it writes, then the shard
        // holding its new name, and the root when it creates that shard.
        let stored = self
            .backend
            .put(&mut &*bytes, "application/vnd.tilo.owner-history+json")
            .map_err(|e| e.to_string())?;
        if stored.hash != address
            || stored.len != u64::try_from(bytes.len()).map_err(|e| e.to_string())?
        {
            return Err("native history backend returned another block".into());
        }
        if self.read(hash)? != bytes {
            return Err("retained native history content differs from its input".into());
        }
        // A block already here returns without that barrier: whoever wrote
        // it may have stopped before it was durable.
        if present {
            std::fs::File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(|e| e.to_string())?;
            tilo_files::native::atomic::sync_directory(&shard).map_err(|e| e.to_string())?;
        }
        // The backend never makes the root's own name durable, nor the names
        // of shards an earlier process created and stopped before flushing.
        // Once per store, before its first root is acknowledged, covers both.
        if !self.root_durable.load(Ordering::Acquire) {
            for directory in [
                self.root.as_path(),
                self.root
                    .parent()
                    .ok_or("history content parent is absent")?,
            ] {
                tilo_files::native::atomic::sync_directory(directory).map_err(|e| e.to_string())?;
            }
            self.root_durable.store(true, Ordering::Release);
        }
        Ok(hash)
    }
    /// Read the complete bounded block, including its content-verification EOF.
    ///
    /// # Errors
    /// Missing, corrupt, oversized or unavailable history.
    pub fn read(&self, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        let reader = self
            .backend
            .open(blake3::Hash::from_bytes(hash).to_hex().as_str())
            .map_err(|e| e.to_string())?
            .ok_or("native owner history block is missing")?;
        let mut bytes = Vec::new();
        reader
            .take(
                u64::try_from(MAX_HISTORY_BLOCK)
                    .map_err(|e| e.to_string())?
                    .checked_add(1)
                    .ok_or("history size overflow")?,
            )
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_HISTORY_BLOCK || *blake3::hash(&bytes).as_bytes() != hash {
            return Err("native owner history block integrity failed".into());
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_history_content_quota_failure_keeps_previously_verified_block() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("content");
        let content = HistoryContent {
            backend: Arc::new(DirBackend::with_limits(
                root.clone(),
                tilo_content::ContentLimits {
                    max_item_bytes: 16,
                    max_total_bytes: 4,
                    max_entries: 2,
                    max_scan_entries: 16,
                },
            )),
            root,
            root_durable: AtomicBool::new(false),
        };
        let accepted = content.put(b"first");
        assert!(accepted.is_err());
        let hash = content.put(b"four").unwrap();
        assert!(content.put(b"more").is_err());
        assert_eq!(content.read(hash).unwrap(), b"four");
        assert_eq!(content.put(b"four").unwrap(), hash);
        let reopened = HistoryContent::for_state_file(&directory.path().join("state.json"));
        assert_eq!(reopened.read(hash).unwrap(), b"four");
    }
}
