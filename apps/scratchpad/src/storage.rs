//! Persistent content-addressed documents with one atomic current reference.

use advisory_lock::{FileLock, LockMode};
use anyhow::{Context, Result};
use automerge::AutoCommit;
use fs::store::ContentStore;
use std::path::{Path, PathBuf};
use tokio::runtime::Runtime;

pub(crate) struct NoteStore {
    store: ContentStore,
    runtime: Runtime,
    current: PathBuf,
    _lock: FileLock,
}

impl NoteStore {
    pub(crate) fn open(directory: &Path) -> Result<(Self, AutoCommit)> {
        std::fs::create_dir_all(directory).context("create note directory")?;
        let lock = FileLock::try_open(&directory.join("lock"), LockMode::Exclusive)
            .context("this note is already open in another instance")?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let store = runtime.block_on(ContentStore::open(directory.join("blobs")))?;
        let storage = Self {
            store,
            runtime,
            current: directory.join("current"),
            _lock: lock,
        };
        let doc = match std::fs::read_to_string(&storage.current) {
            Ok(reference) => {
                // The app writes the content store's BLAKE3 hash in hex.
                let hash = blake3::Hash::from_hex(reference.trim())
                    .context("invalid saved note reference")?;
                let bytes = storage
                    .runtime
                    .block_on(storage.store.local(hash.into()))
                    .context("saved note content is unavailable")?;
                AutoCommit::load(&bytes).context("saved note content is corrupt")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => AutoCommit::new(),
            Err(error) => return Err(error).context("read saved note reference"),
        };
        Ok((storage, doc))
    }

    pub(crate) fn save(&self, doc: &AutoCommit) -> Result<()> {
        // Serialization may commit pending changes; never mutate the host's
        // document from an effect callback, which receives it read-only.
        let bytes = doc.clone().save();
        // ContentStore::ingest installs and syncs a persistent protection tag
        // before returning. Publish its reference only after that succeeds.
        let hash = self.runtime.block_on(self.store.ingest(bytes))?;
        atomic_file::write(&self.current, format!("{hash}\n").as_bytes())
            .context("publish saved note reference")
    }
}

impl Drop for NoteStore {
    fn drop(&mut self) {
        if let Err(error) = self.runtime.block_on(self.store.store().shutdown()) {
            eprintln!("scratchpad: storage shutdown failed: {error}");
        }
    }
}
