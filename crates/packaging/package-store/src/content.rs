//! The content store, driven synchronously.
//!
//! Package payload blobs live in the fs crate's [`ContentStore`] — the same
//! implementation, format and BLAKE3 hash space as every other blob on the
//! device — under `<store>/content`. The store is a single-writer database,
//! so it is opened for one operation at a time, under the package store's
//! lock, and closed again; `tilo-install` and the app host therefore never
//! contend for it beyond that lock.
//!
//! Callers are synchronous (a CLI, a daemon's blocking pool), and may
//! themselves be running inside an async runtime, so each session runs on a
//! scoped thread with its own current-thread runtime.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use fs::store::ContentStore;
use iroh_blobs::Hash;
use tilo_app_package::BlobHash;

use crate::StoreError;

/// Blobs no larger than this are read into memory to ingest; larger ones
/// are staged through a file so memory stays flat.
const SMALL_BLOB: u64 = 256 * 1024;

pub(crate) struct Content<'a> {
    runtime: &'a tokio::runtime::Runtime,
    store: &'a ContentStore,
    staging: &'a Path,
}

fn hash(blob: &BlobHash) -> Hash {
    Hash::from_bytes(*blob.as_bytes())
}

fn content_error(error: impl std::fmt::Display) -> StoreError {
    StoreError::Content(error.to_string())
}

impl Content<'_> {
    /// Length of the complete blob, or `None` when absent or partial.
    pub(crate) fn len(&self, blob: &BlobHash) -> Result<Option<u64>, StoreError> {
        self.runtime
            .block_on(self.store.complete_len(hash(blob)))
            .map_err(content_error)
    }

    /// Ingest `len` bytes from `reader`, returning the hash they actually
    /// have. The caller compares it with the name it expected.
    pub(crate) fn ingest(&self, len: u64, reader: &mut dyn Read) -> Result<BlobHash, StoreError> {
        let stored = if len <= SMALL_BLOB {
            let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
            reader.take(len).read_to_end(&mut bytes)?;
            self.runtime
                .block_on(self.store.ingest(bytes::Bytes::from(bytes)))
                .map_err(content_error)?
        } else {
            // The store copies the staged bytes in and the file is gone
            // right after, so it needs no durability of its own.
            let staged = tempfile::NamedTempFile::new_in(self.staging)?;
            std::io::copy(&mut reader.take(len), &mut staged.as_file())?;
            self.runtime
                .block_on(self.store.ingest_file(staged.path()))
                .map_err(content_error)?
        };
        Ok(BlobHash(*stored.as_bytes()))
    }

    /// Run `work`, a batch of ingests, with one database sync at its end
    /// instead of one per blob. The sync runs whatever `work` returned;
    /// nothing the batch wrote counts as stored until this returns `Ok`.
    pub(crate) fn deferred<R>(
        &self,
        work: impl FnOnce() -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        self.store.defer_sync();
        let result = work();
        let synced = self
            .runtime
            .block_on(self.store.end_deferred_sync())
            .map_err(content_error);
        let value = result?;
        synced?;
        Ok(value)
    }

    /// Database syncs this session's writes have made; see
    /// [`ContentStore::write_syncs`].
    #[cfg(test)]
    pub(crate) fn write_syncs(&self) -> u64 {
        self.store.write_syncs()
    }

    /// Write `blob` to the new file `target`, sharing extents with the
    /// store's copy where the filesystem allows.
    pub(crate) fn export(&self, blob: &BlobHash, target: &Path) -> Result<(), StoreError> {
        self.runtime
            .block_on(self.store.export_file(hash(blob), target))
            .map(drop)
            .map_err(content_error)
    }

    /// The complete blob `blob` as an open file, or `None` when the store
    /// lacks it. The file is a private copy that is gone once closed.
    pub(crate) fn open(&self, blob: &BlobHash) -> Result<Option<(u64, std::fs::File)>, StoreError> {
        let Some(len) = self.len(blob)? else {
            return Ok(None);
        };
        let dir = tempfile::tempdir_in(self.staging)?;
        let target = dir.path().join("blob");
        self.export(blob, &target)?;
        let file = std::fs::File::open(&target)?;
        Ok(Some((len, file)))
    }

    /// Every blob held, complete or partial.
    pub(crate) fn hashes(&self) -> Result<Vec<BlobHash>, StoreError> {
        Ok(self
            .runtime
            .block_on(self.store.blob_hashes())
            .map_err(content_error)?
            .into_iter()
            .map(|hash| BlobHash(*hash.as_bytes()))
            .collect())
    }

    /// Remove exactly `selected`, keeping every other blob.
    pub(crate) fn evict(&self, selected: &[BlobHash]) -> Result<(), StoreError> {
        if selected.is_empty() {
            return Ok(());
        }
        let selected: HashSet<Hash> = selected.iter().map(hash).collect();
        self.runtime
            .block_on(self.store.evict_blobs(&selected))
            .map(drop)
            .map_err(content_error)
    }
}

/// Drain complete imports and commit their metadata before registry publication.
/// Package sessions do not write partial-download handles that need a shutdown
/// bitfield write. The caller holds the exclusive package-store lock.
pub(crate) fn flush(content: &Content<'_>) -> Result<(), StoreError> {
    content.runtime.block_on(async {
        content
            .store
            .store()
            .wait_idle()
            .await
            .map_err(content_error)?;
        content.store.store().sync_db().await.map_err(content_error)
    })
}

/// Open the content store in `dir` for one session, run `work`, and close
/// the store whatever `work` returned.
pub(crate) fn session<R: Send>(
    dir: &Path,
    staging: &Path,
    work: impl FnOnce(&Content<'_>) -> Result<R, StoreError> + Send,
) -> Result<R, StoreError> {
    session_with_close(dir, staging, work, |_, closed| closed)
}

pub(crate) fn session_with_close<R: Send>(
    dir: &Path,
    staging: &Path,
    work: impl FnOnce(&Content<'_>) -> Result<R, StoreError> + Send,
    after_close: impl FnOnce(Option<&R>, Result<(), StoreError>) -> Result<(), StoreError> + Send,
) -> Result<R, StoreError> {
    let dir: PathBuf = dir.to_path_buf();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                std::fs::create_dir_all(staging)?;
                let store = runtime
                    .block_on(ContentStore::open(&dir))
                    .map_err(content_error)?;
                let result = work(&Content {
                    runtime: &runtime,
                    store: &store,
                    staging,
                });
                let closed = runtime.block_on(store.close()).map_err(content_error);
                let closed = after_close(result.as_ref().ok(), closed);
                let value = result?;
                closed?;
                Ok(value)
            })
            .join()
            .unwrap_or_else(|_| Err(StoreError::Content("content-store session panicked".into())))
    })
}
