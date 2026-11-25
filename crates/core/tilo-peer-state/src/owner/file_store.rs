//! The record as an index and one snapshot per workspace, beside the state
//! file of the host that keeps it, kept as [`Journaled`] keeps a record: the
//! caller holds exclusive access to the directory. A host that keeps state of
//! its own derived from the record (its workspaces of the workspaces) follows
//! each publication through a [`FileHost`].
//!
//! A publication writes what it changed, never the record whole, so a device
//! holding many workspaces pays for the one it touched, and a workspace for
//! the change: an entry appended to its snapshot's journal, or a snapshot
//! written again with the chunks of its replica that changed. The index is
//! the commit point of a change, and a file the index does not name is not
//! the record's — a publication that stopped before it leaves the record as
//! it was, and the next open lets the files it left go.
//!
//! Each part is a file of its own in a directory with names of its own
//! ([`parts_of`], `index.json` in it), so a record's files never share the
//! name of the file the host keeps them beside.
use crate::owner::journaled::{INDEX_NAME, JournalParts, Journaled};
use crate::owner::{Change, DeviceWorkspaces, NativeAuthorityStorage, Store};
use parking_lot::{Mutex, RwLock};
use std::{
    collections::BTreeSet,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tilo_desktop_model::WorkspaceId;

/// The file a host whose own state is the file `state` keeps its owner's
/// record in, beside it (`state.json` keeps it in `state.owner.json`).
#[must_use]
pub fn owner_file_of(state: &Path) -> PathBuf {
    state.with_extension("owner.json")
}

/// What a host that keeps state derived from the record does around each
/// publication of it.
pub trait FileHost: std::any::Any + Send + Sync {
    /// Whether the host's shell holds `workspace`.
    fn shows(&self, _workspace: WorkspaceId) -> bool {
        false
    }
    /// Run `write`, which makes `next` durable and installs it, with
    /// whatever the host keeps beside the record: it may refuse the
    /// publication before calling `write`, and follows `change` once
    /// `write` succeeded. Nothing is installed when `write` is not called
    /// or fails.
    ///
    /// # Errors
    /// The publication is refused, or `write` failed.
    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change,
        write: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let _ = (next, change);
        write()
    }
}

struct NoHost;
impl FileHost for NoHost {}

/// The directory beside the record file that holds the record: its index and
/// one file per workspace.
pub(crate) fn parts_of(path: &Path) -> PathBuf {
    path.with_extension("workspaces")
}

/// The record's index, in the directory beside the record file.
pub(crate) fn index_of(parts: &Path) -> PathBuf {
    parts.join("index.json")
}

/// The file the record's part `name` is kept in: the index, and each
/// workspace's snapshot (`<workspace>.<revision>.json`), its journal
/// (`.log`), the list of its chunks (`.chunks`) and each chunk
/// (`<workspace>.<digest>.chunk`).
fn file_of(parts: &Path, name: &str) -> PathBuf {
    if name == INDEX_NAME {
        return index_of(parts);
    }
    let mut kind = name.chars();
    let extension = match kind.next() {
        Some('w') => "json",
        Some('l') => "log",
        Some('k') => "chunks",
        _ => "chunk",
    };
    parts.join(format!("{}.{extension}", kind.as_str()))
}

/// Every file of the record at `path` as the disk holds it — the index and
/// each workspace's files — sorted by path.
#[cfg(test)]
pub(crate) fn document_files(path: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>, std::io::Error> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(parts_of(path)) else {
        return Ok(files);
    };
    for entry in entries {
        let entry = entry?;
        files.push((entry.path(), std::fs::read(entry.path())?));
    }
    files.sort();
    Ok(files)
}

fn provision(parts: &Path) -> Result<(), String> {
    if parts.is_dir() {
        return Ok(());
    }
    #[cfg(unix)]
    tilo_files::native::atomic::provision_directory(
        parts,
        tilo_files::native::atomic::Privacy::OwnerOnly,
    )
    .map_err(|error| error.into_io_error().to_string())?;
    // Windows has no durable directory provisioning; the private directory carries the same
    // owner-only access through its ACL.
    #[cfg(not(unix))]
    tilo_files::native::private::directory(parts).map_err(|error| error.to_string())?;
    Ok(())
}

/// Read the file at `path`, or `None` when there is none. A workspace's
/// files hold every document of the workspace, so no cap is put on them: one
/// would be reached by using the workspace.
fn read_whole(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// Compact JSON of what this device holds of one workspace.
#[cfg(test)]
pub(super) fn part_bytes(
    stored: &crate::owner::stored::StoredWorkspace,
) -> Result<Vec<u8>, String> {
    serde_json::to_vec(stored).map_err(|error| error.to_string())
}

/// The record's parts as files in `parts`, written by `store` publishing
/// `next`; read-only without one.
struct FileParts<'a> {
    parts: &'a Path,
    writing: Option<(&'a FileStore, &'a NativeAuthorityStorage)>,
}

impl FileParts<'_> {
    fn writing(&self) -> Result<(&FileStore, &NativeAuthorityStorage), String> {
        self.writing
            .ok_or_else(|| "the record's files are read here, not written".into())
    }
}

impl JournalParts for FileParts<'_> {
    fn read_part(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        read_whole(&file_of(self.parts, name))
    }

    fn write_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let (store, next) = self.writing()?;
        provision(self.parts)?;
        store.replace(&file_of(self.parts, name), bytes, next)
    }

    fn bring_up_part(&self, name: &str, bytes: Vec<u8>, stored: u32) -> Result<Vec<u8>, String> {
        super::stored::format::FORMAT
            .bring_up(&file_of(self.parts, name), bytes, stored)
            .map_err(|error| error.to_string())
    }

    fn append_part(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let (store, next) = self.writing()?;
        provision(self.parts)?;
        if let Err(error) = tilo_files::native::atomic::append(&file_of(self.parts, name), bytes) {
            if error.state() != tilo_files::native::atomic::PublicationState::NotPublished {
                // Some of the entry may be on disk: the record is read back
                // before anything else is published.
                store.fence(next);
            }
            return Err(error.into_io_error().to_string());
        }
        #[cfg(test)]
        store.written.fetch_add(bytes.len(), Ordering::Relaxed);
        Ok(())
    }

    fn remove_part(&self, name: &str) -> Result<(), String> {
        match std::fs::remove_file(file_of(self.parts, name)) {
            Err(error) if error.kind() != ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        }
    }
}

/// The record kept at `path`, empty when there is none, and the journaled
/// form of it the next publication writes in.
fn read_resource(path: &Path) -> Result<(Journaled, NativeAuthorityStorage), String> {
    let parts = parts_of(path);
    let reading = FileParts {
        parts: &parts,
        writing: None,
    };
    Ok(Journaled::open(&reading)?.unwrap_or_else(|| {
        let empty = NativeAuthorityStorage::default();
        (Journaled::start(&empty), empty)
    }))
}

/// Read the record kept at `path`: the index beside it and the workspace
/// files the index names. Empty when there is none.
pub fn read_file(path: &Path) -> Result<NativeAuthorityStorage, String> {
    read_resource(path).map(|(_, record)| record)
}

/// Open the owner whose record is the file at `path`, starting empty.
pub fn open_file(path: &Path) -> Result<DeviceWorkspaces, String> {
    open_file_with(path, Arc::new(NoHost))
}

/// [`open_file`] for a host that follows each publication through `host`.
pub fn open_file_with(path: &Path, host: Arc<dyn FileHost>) -> Result<DeviceWorkspaces, String> {
    let (mut journaled, authority) = read_resource(path)?;
    let store = Arc::new(FileStore::at(path, host));
    let owner = DeviceWorkspaces::restore(
        Arc::clone(&store) as Arc<dyn Store>,
        authority,
        tilo_clock::wall(),
    )?;
    // A write that changes neither the index nor the workspaces held appends
    // to the journal it read.
    journaled.resume(&owner);
    *store.journaled.lock() = journaled;
    // A publication that stopped before the index left files the record does
    // not name.
    store.prune_parts();
    Ok(owner)
}

pub(crate) struct FileStore {
    pub(crate) host: Arc<dyn FileHost>,
    parts: PathBuf,
    history: crate::owner::HistoryContent,
    /// The record as its files keep it, as this store read or last wrote it.
    journaled: Mutex<Journaled>,
    /// Every byte this store wrote.
    #[cfg(test)]
    pub(crate) written: std::sync::atomic::AtomicUsize,
    healthy: AtomicBool,
    uncertain: RwLock<BTreeSet<WorkspaceId>>,
}

impl FileStore {
    fn at(path: &Path, host: Arc<dyn FileHost>) -> Self {
        Self {
            host,
            parts: parts_of(path),
            history: crate::owner::HistoryContent::for_state_file(path),
            journaled: Mutex::new(Journaled::start(&NativeAuthorityStorage::default())),
            #[cfg(test)]
            written: std::sync::atomic::AtomicUsize::new(0),
            healthy: AtomicBool::new(true),
            uncertain: RwLock::new(BTreeSet::new()),
        }
    }

    /// Fence the store after a write whose publication may have happened: the
    /// record the disk holds is read back before anything else is published.
    fn fence(&self, next: &NativeAuthorityStorage) {
        self.uncertain.write().extend(next.workspaces());
        self.healthy.store(false, Ordering::Release);
    }

    /// Replace a file of the record durably.
    fn replace(
        &self,
        path: &Path,
        bytes: &[u8],
        next: &NativeAuthorityStorage,
    ) -> Result<(), String> {
        match tilo_files::native::atomic::replace(
            path,
            bytes,
            tilo_files::native::atomic::Privacy::OwnerOnly,
        ) {
            Ok(()) => {
                #[cfg(test)]
                self.written.fetch_add(bytes.len(), Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                if error.state() != tilo_files::native::atomic::PublicationState::NotPublished {
                    self.fence(next);
                }
                Err(error.into_io_error().to_string())
            }
        }
    }

    /// Remove the files the record as published does not name: those a publication stopped
    /// before its index wrote.
    fn prune_parts(&self) {
        let Ok(entries) = std::fs::read_dir(&self.parts) else {
            return;
        };
        let kept: BTreeSet<PathBuf> = self
            .journaled
            .lock()
            .named()
            .iter()
            .map(|name| file_of(&self.parts, name))
            .collect();
        for entry in entries.flatten() {
            let path = entry.path();
            if kept.contains(&path) {
                continue;
            }
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != ErrorKind::NotFound
            {
                tracing::warn!(%error, "a workspace file the record does not name was kept");
            }
        }
    }

    /// Write what `change` changed and only that, then install `next`.
    fn write(
        &self,
        next: &NativeAuthorityStorage,
        change: &Change,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        let parts = FileParts {
            parts: &self.parts,
            writing: Some((self, next)),
        };
        let indexed = self
            .journaled
            .lock()
            .publish_indexed(&parts, next, change)?;
        install();
        // Once the index moved, what it named before is not the record's.
        if indexed {
            self.prune_parts();
        }
        Ok(())
    }
}

impl Store for FileStore {
    fn put_history(&self, bytes: &[u8]) -> Result<[u8; 32], String> {
        self.history.put(bytes)
    }
    fn read_history(&self, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        self.history.read(hash)
    }
    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn uncertain(&self, workspace: WorkspaceId) -> bool {
        self.uncertain.read().contains(&workspace)
    }

    fn shows(&self, workspace: WorkspaceId) -> bool {
        self.host.shows(workspace)
    }

    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        if !self.healthy() {
            return Err("joined-workspace store is fenced after an uncertain publication".into());
        }
        let written = match &change {
            Change::Pending => Change::Pending,
            Change::Workspace { workspace, shown } => Change::Workspace {
                workspace: *workspace,
                shown: *shown,
            },
            Change::Released { workspace } => Change::Released {
                workspace: *workspace,
            },
        };
        self.host
            .publish(next, change, &mut || self.write(next, &written, install))
    }
}

#[cfg(test)]
#[path = "file_store_tests.rs"]
mod tests;
