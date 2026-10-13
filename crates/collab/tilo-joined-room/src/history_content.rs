//! The owner's immutable history uses the existing durable content backend.
use std::{
    io::Read as _,
    path::{Path, PathBuf},
    sync::Arc,
};
use tilo_content::{ContentBackend, DirBackend};

/// Native document snapshots share this backend with smaller receipt/index blocks.
const MAX_HISTORY_BLOCK: usize = 3 * 1024 * 1024;

/// Durable content custody shared by owner stores using the same state directory.
/// This is not an authority: an admitted signed room root authenticates references.
pub struct HistoryContent {
    backend: Arc<dyn ContentBackend>,
    root: PathBuf,
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
        let stored = self
            .backend
            .put(&mut &*bytes, "application/vnd.tilo.owner-history+json")
            .map_err(|e| e.to_string())?;
        if stored.hash != blake3::Hash::from_bytes(hash).to_hex().as_str()
            || stored.len != u64::try_from(bytes.len()).map_err(|e| e.to_string())?
        {
            return Err("native history backend returned another block".into());
        }
        if self.read(hash)? != bytes {
            return Err("retained native history content differs from its input".into());
        }
        let address = blake3::Hash::from_bytes(hash).to_hex().to_string();
        let shard = self
            .root
            .join(address.get(..2).ok_or("invalid content address")?);
        // DirBackend verifies/streams the bytes; this owner additionally needs
        // durable names, including new shard/root directory entries, before its
        // state-file root can be acknowledged. Idempotent puts take this same barrier.
        std::fs::File::open(shard.join(&address))
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        for directory in [
            shard.as_path(),
            self.root.as_path(),
            self.root
                .parent()
                .ok_or("history content parent is absent")?,
        ] {
            tilo_files::native::atomic::sync_directory(directory).map_err(|e| e.to_string())?;
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
