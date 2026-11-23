//! Whole files in and out of the content store.
//!
//! Package payloads are ordinary files that must end up as ordinary files
//! again: an app is executed from a directory tree, not read through the
//! store. These calls move file content across that boundary without
//! staging it in memory. Import hashes the file while copying it into the
//! store, and export writes a blob to a path by cloning the store's own
//! file (`FICLONE`) when both live on one reflink-capable filesystem, so a
//! materialised tree shares extents with the store instead of doubling the
//! bytes. Other filesystems get a plain copy.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use iroh_blobs::Hash;

use super::ContentStore;

impl ContentStore {
    /// Import the file at `path` and return its BLAKE3 content hash.
    ///
    /// The blob is stored like [`ContentStore::ingest`], so it survives until it is explicitly
    /// evicted. The source file is left untouched. Like `ingest`, the import is durable when this
    /// returns or, inside a deferred-durability group ([`ContentStore::defer_sync`]), when the group
    /// ends: importing a tree of files then costs one database sync rather than one per file.
    ///
    /// # Errors
    ///
    /// Returns an error if `path` is not absolute, cannot be read, or the store rejects the import.
    pub async fn ingest_file(&self, path: &Path) -> Result<Hash> {
        anyhow::ensure!(
            path.is_absolute(),
            "content-store import needs an absolute path, got {}",
            path.display()
        );
        let _mutation = self.mutation.lock().await;
        let files = Arc::clone(&self.files);
        let source_path = path.to_owned();
        let hash = tokio::task::spawn_blocking(move || -> std::io::Result<Hash> {
            let source = File::open(&source_path)?;
            let len = source.metadata()?.len();
            let digest = crate::store::area::hash_file(&source_path)?;
            if len <= super::INLINE_MAX as u64 {
                let bytes = std::fs::read(&source_path)?;
                files.put_inline([(digest, bytes::Bytes::from(bytes))])?;
            } else {
                let mut reader = File::open(&source_path)?;
                let outboard = super::outboard_of_reader(&mut reader, len)?;
                files.adopt_blob_clone(&digest, &source, len, &outboard)?;
            }
            Ok(Hash::from(digest))
        })
        .await
        .with_context(|| format!("import {}", path.display()))??;
        self.note_write_durability();
        Ok(hash)
    }

    /// Write the complete blob `hash` to `target`, creating parent
    /// directories. `target` must not exist yet.
    ///
    /// The store clones its own file into `target` when the filesystem can
    /// share extents, and copies otherwise; either way the result is a
    /// separate file, so later writes to it never reach the store.
    pub async fn export_file(&self, hash: Hash, target: &Path) -> Result<u64> {
        anyhow::ensure!(
            target.is_absolute(),
            "content-store export needs an absolute path, got {}",
            target.display()
        );
        anyhow::ensure!(
            std::fs::symlink_metadata(target).is_err(),
            "refusing to overwrite {}",
            target.display()
        );
        let files = Arc::clone(&self.files);
        let path = target.to_owned();
        let len = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
            let key = *hash.as_bytes();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut dest = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            if let Some(bytes) = files.get_inline(&key) {
                dest.write_all(&bytes)?;
                return Ok(bytes.len() as u64);
            }
            if let Some(location) = files.find_chunk(&key) {
                let bytes =
                    files.read_at(&location.key, location.offset, u64::from(location.len))?;
                dest.write_all(&bytes)?;
                return Ok(bytes.len() as u64);
            }
            let entry = files
                .entry(&key)
                .filter(super::area::EntryInfo::complete)
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "no complete such blob")
                })?;
            files.copy_to(&key, &(0..entry.size), &dest)?;
            Ok(entry.size)
        })
        .await
        .with_context(|| format!("export {hash} to {}", target.display()))??;
        Ok(len)
    }

    /// Size of the complete blob `hash`, or `None` when the store holds no
    /// complete copy of it.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot answer.
    pub async fn complete_len(&self, hash: Hash) -> Result<Option<u64>> {
        Ok(match self.blob_status(hash).await? {
            iroh_blobs::api::blobs::BlobStatus::Complete { size } => Some(size),
            iroh_blobs::api::blobs::BlobStatus::NotFound
            | iroh_blobs::api::blobs::BlobStatus::Partial { .. } => None,
        })
    }

    /// Every blob the store currently holds: inline blobs, whole blobs and the chunks of
    /// file-backed versions.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot enumerate its contents.
    pub async fn blob_hashes(&self) -> Result<Vec<Hash>> {
        let files = Arc::clone(&self.files);
        let hashes = tokio::task::spawn_blocking(move || {
            let mut hashes: Vec<Hash> = files
                .inline_keys()
                .into_iter()
                .chain(files.blob_keys())
                .chain(files.chunk_hashes())
                .map(Hash::from)
                .collect();
            hashes.sort_unstable_by_key(|hash| *hash.as_bytes());
            hashes.dedup();
            hashes
        })
        .await?;
        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_lock::MutexRecover as _;

    #[tokio::test]
    async fn files_round_trip_through_a_reopenable_store_without_clobbering_exports() {
        {
            let dir = tempfile::tempdir().unwrap();
            let store = ContentStore::open(dir.path().join("store")).await.unwrap();
            let source = dir.path().join("source.bin");
            let bytes: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
            std::fs::write(&source, &bytes).unwrap();

            let hash = store.ingest_file(&source).await.unwrap();
            assert_eq!(hash, Hash::new(&bytes));
            assert_eq!(
                store.complete_len(hash).await.unwrap(),
                Some(bytes.len() as u64)
            );
            assert!(store.blob_hashes().await.unwrap().contains(&hash));

            let target = dir.path().join("out/nested/copy.bin");
            let written = store.export_file(hash, &target).await.unwrap();
            assert_eq!(written, bytes.len() as u64);
            assert_eq!(std::fs::read(&target).unwrap(), bytes);

            // The export is its own file: changing it leaves the stored blob intact.
            std::fs::write(&target, b"changed").unwrap();
            let again = dir.path().join("again.bin");
            store.export_file(hash, &again).await.unwrap();
            assert_eq!(std::fs::read(&again).unwrap(), bytes);
            store.close().await.unwrap();
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let store = ContentStore::open(dir.path().join("store")).await.unwrap();
            let hash = store.ingest(bytes::Bytes::from_static(b"x")).await.unwrap();
            let target = dir.path().join("taken");
            std::fs::write(&target, b"mine").unwrap();
            assert!(store.export_file(hash, &target).await.is_err());
            assert_eq!(std::fs::read(&target).unwrap(), b"mine");
            store.close().await.unwrap();
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("store");
            let store = ContentStore::open(&root).await.unwrap();
            let source = dir.path().join("f");
            std::fs::write(&source, vec![7u8; 3 * 1024 * 1024]).unwrap();
            let hash = store.ingest_file(&source).await.unwrap();
            store.close().await.unwrap();

            let reopened = ContentStore::open(&root).await.unwrap();
            assert_eq!(
                reopened.complete_len(hash).await.unwrap(),
                Some(3 * 1024 * 1024)
            );
            assert_eq!(
                reopened.complete_len(Hash::new(b"absent")).await.unwrap(),
                None
            );
            reopened.close().await.unwrap();
        }
    }

    /// Open deferred groups, and whether one still owes a database sync.
    fn owed_sync(store: &ContentStore) -> (usize, bool) {
        *store.deferred.lock_recover()
    }

    #[tokio::test]
    async fn file_imports_in_a_deferred_group_sync_once_when_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        let store = ContentStore::open(&root).await.unwrap();
        let mut imported = Vec::new();
        store.defer_sync();
        for n in 0..3u8 {
            let source = dir.path().join(format!("f{n}"));
            std::fs::write(&source, vec![n; 70_000]).unwrap();
            imported.push(store.ingest_file(&source).await.unwrap());
            assert_eq!(owed_sync(&store), (1, true));
        }
        store.end_deferred_sync().await.unwrap();
        assert_eq!(owed_sync(&store), (0, false));

        let source = dir.path().join("alone");
        std::fs::write(&source, vec![9u8; 70_000]).unwrap();
        imported.push(store.ingest_file(&source).await.unwrap());
        assert_eq!(owed_sync(&store), (0, false));
        store.close().await.unwrap();

        let reopened = ContentStore::open(&root).await.unwrap();
        for hash in imported {
            assert_eq!(reopened.complete_len(hash).await.unwrap(), Some(70_000));
        }
        reopened.close().await.unwrap();
    }
}
