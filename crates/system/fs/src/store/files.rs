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

use std::path::Path;

use anyhow::{Context, Result};
use iroh_blobs::api::blobs::{AddPathOptions, BlobStatus, ImportMode};
use iroh_blobs::{BlobFormat, Hash};

use super::{ContentStore, owned_blob_tag};

impl ContentStore {
    /// Import the file at `path` and return its BLAKE3 content hash.
    ///
    /// The blob is protected by the same service-owned tag as
    /// [`ContentStore::ingest`], so it survives until it is explicitly
    /// evicted. The source file is left untouched. Like `ingest`, the import
    /// is durable when this returns or, inside a deferred-durability group
    /// ([`ContentStore::defer_sync`]), when the group ends: importing a tree
    /// of files then costs one database sync rather than one per file.
    ///
    /// # Errors
    ///
    /// Returns an error if `path` is not absolute, cannot be read, or the
    /// store rejects the import.
    pub async fn ingest_file(&self, path: &Path) -> Result<Hash> {
        anyhow::ensure!(
            path.is_absolute(),
            "content-store import needs an absolute path, got {}",
            path.display()
        );
        let _mutation = self.mutation.lock().await;
        let _owned_tags = self.owned_tags.lock().await;
        let temporary = self
            .store()
            .blobs()
            .add_path_with_opts(AddPathOptions {
                path: path.to_path_buf(),
                format: BlobFormat::Raw,
                mode: ImportMode::Copy,
            })
            .temp_tag()
            .await
            .with_context(|| format!("import {}", path.display()))?;
        let hash = temporary.hash();
        self.store()
            .tags()
            .set(
                owned_blob_tag(hash, temporary.format()),
                temporary.hash_and_format(),
            )
            .await?;
        self.sync_unless_deferred().await?;
        drop(temporary);
        Ok(hash)
    }

    /// Write the complete blob `hash` to `target`, creating parent
    /// directories. `target` must not exist yet.
    ///
    /// The store clones its own file into `target` when the filesystem can
    /// share extents, and copies otherwise; either way the result is a
    /// separate file, so later writes to it never reach the store.
    ///
    /// # Errors
    ///
    /// Returns an error if `target` is not absolute or already exists, the
    /// blob is absent or incomplete, or the write fails.
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
        self.store()
            .blobs()
            .export(hash, target)
            .await
            .with_context(|| format!("export {hash} to {}", target.display()))
    }

    /// Size of the complete blob `hash`, or `None` when the store holds no
    /// complete copy of it.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot answer.
    pub async fn complete_len(&self, hash: Hash) -> Result<Option<u64>> {
        match self.store().blobs().status(hash).await? {
            BlobStatus::Complete { size } => Ok(Some(size)),
            BlobStatus::NotFound | BlobStatus::Partial { .. } => Ok(None),
        }
    }

    /// Every blob the store currently holds, complete or partial.
    ///
    /// # Errors
    ///
    /// Returns an error if the store cannot enumerate its contents.
    pub async fn blob_hashes(&self) -> Result<Vec<Hash>> {
        Ok(self.store().blobs().list().hashes().await?)
    }

    /// Flush and close the store so another process may open the same
    /// directory. Pending writes are durable once this returns.
    ///
    /// # Errors
    ///
    /// Returns an error if the final flush fails.
    pub async fn close(self) -> Result<()> {
        self.store().sync_db().await?;
        self.store().shutdown().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_lock::MutexRecover as _;

    #[tokio::test]
    async fn files_round_trip_through_a_reopenable_store_without_clobbering_exports() {
        // Phase `a_file_round_trips_through_the_store`.
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
        // Phase `export_refuses_to_replace_an_existing_file`.
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
        // Phase `a_closed_store_can_be_reopened_with_its_blobs`.
        {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("store");
            let store = ContentStore::open(&root).await.unwrap();
            let source = dir.path().join("f");
            std::fs::write(&source, vec![7u8; 70_000]).unwrap();
            let hash = store.ingest_file(&source).await.unwrap();
            store.close().await.unwrap();

            let reopened = ContentStore::open(&root).await.unwrap();
            assert_eq!(reopened.complete_len(hash).await.unwrap(), Some(70_000));
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
            // The import left its sync to the group.
            assert_eq!(owed_sync(&store), (1, true));
        }
        store.end_deferred_sync().await.unwrap();
        assert_eq!(owed_sync(&store), (0, false));

        // Outside a group an import owes nothing once it returns.
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
