//! The record in a file of its own. The caller holds exclusive access to
//! the file's directory. A host that keeps state of its own derived from
//! the record (its workspaces of the rooms) follows each publication
//! through a [`FileHost`].
use crate::{
    Change, LimitedWriter, MAX_AUTHORITY_BYTES, NativeAuthorityStorage, NativeWorkspaceOwner,
    RoomId, Store, WorkspaceId, preflight_storage,
};
use parking_lot::RwLock;
use std::{
    collections::BTreeSet,
    io::Read as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

/// The file a host whose own state is the file `state` keeps its owner's
/// record in, beside it (`state.json` keeps it in `state.owner.json`).
#[must_use]
pub fn owner_file_of(state: &Path) -> PathBuf {
    state.with_extension("owner.json")
}

/// What a host that keeps state derived from the record does around each
/// publication of it.
pub trait FileHost: std::any::Any + Send + Sync {
    /// The room a local workspace is, where the host has workspaces.
    fn slot_room(&self, _slot: WorkspaceId) -> Option<RoomId> {
        None
    }
    /// Run `write`, which makes `next` durable and installs it, with
    /// whatever the host keeps beside the record: it may refuse the
    /// publication before calling `write`, and follows `change` once
    /// `write` succeeded. Nothing is installed when `write` is not called
    /// or fails.
    ///
    /// # Errors
    /// The host refuses the publication, or `write` failed.
    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        write: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let _ = (next, change);
        write()
    }
}

struct NoHost;
impl FileHost for NoHost {}

/// Read the record the file at `path` holds, empty when there is none.
///
/// # Errors
/// The file cannot be read or holds an invalid record.
pub fn read_file(path: &Path) -> Result<NativeAuthorityStorage, String> {
    match std::fs::File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take((MAX_AUTHORITY_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > MAX_AUTHORITY_BYTES {
                return Err("joined-room record exceeds its cap".into());
            }
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(NativeAuthorityStorage::default())
        }
        Err(error) => Err(error.to_string()),
    }
}

/// Make `record` the file's whole content, as the owner's publications write
/// it: how a record kept somewhere else (a desktop state file from before the
/// owner had a file of its own) reaches this one.
///
/// # Errors
/// The record fails its preflight, or the file cannot be replaced.
pub fn write_file(path: &Path, record: &NativeAuthorityStorage) -> Result<(), String> {
    preflight_storage(record)?;
    // Compact JSON, as [`Store::publish`] writes it.
    let mut bytes = LimitedWriter::new(Vec::new(), MAX_AUTHORITY_BYTES);
    serde_json::to_writer(&mut bytes, record).map_err(|e| e.to_string())?;
    tilo_files::native::atomic::replace(
        path,
        &bytes.into_inner(),
        tilo_files::native::atomic::Privacy::OwnerOnly,
    )
    .map_err(|error| error.into_io_error().to_string())
}

/// Open the owner whose record is the file at `path`, starting empty.
///
/// # Errors
/// The file cannot be read or holds an invalid record.
pub fn open_file(path: &Path) -> Result<NativeWorkspaceOwner, String> {
    open_file_with(path, Arc::new(NoHost))
}

/// [`open_file`] for a host that follows each publication through `host`.
///
/// # Errors
/// The file cannot be read or holds an invalid record.
pub fn open_file_with(
    path: &Path,
    host: Arc<dyn FileHost>,
) -> Result<NativeWorkspaceOwner, String> {
    let authority = read_file(path)?;
    let store = FileStore {
        host,
        path: path.to_path_buf(),
        history: crate::HistoryContent::for_state_file(path),
        healthy: AtomicBool::new(true),
        uncertain: RwLock::new(BTreeSet::new()),
    };
    NativeWorkspaceOwner::restore(Arc::new(store), authority, SystemTime::now())
}

pub(crate) struct FileStore {
    pub(crate) host: Arc<dyn FileHost>,
    path: PathBuf,
    history: crate::HistoryContent,
    healthy: AtomicBool,
    uncertain: RwLock<BTreeSet<RoomId>>,
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

    fn uncertain(&self, room: RoomId) -> bool {
        self.uncertain.read().contains(&room)
    }

    fn slot_room(&self, slot: WorkspaceId) -> Option<RoomId> {
        self.host.slot_room(slot)
    }

    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        if !self.healthy() {
            return Err("joined-room store is fenced after an uncertain publication".into());
        }
        preflight_storage(next)?;
        self.host
            .publish(next, change, &mut || self.write(next, install))
    }
}

impl FileStore {
    fn write(
        &self,
        next: &NativeAuthorityStorage,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        // A machine file: compact JSON. The cap and its preflight still count
        // the pretty-printed form, so the compact file always fits it.
        let mut bytes = LimitedWriter::new(Vec::new(), MAX_AUTHORITY_BYTES);
        serde_json::to_writer(&mut bytes, next).map_err(|e| e.to_string())?;
        match tilo_files::native::atomic::replace(
            &self.path,
            &bytes.into_inner(),
            tilo_files::native::atomic::Privacy::OwnerOnly,
        ) {
            Ok(()) => {
                install();
                Ok(())
            }
            Err(error) => {
                if error.state() != tilo_files::native::atomic::PublicationState::NotPublished {
                    self.uncertain.write().extend(next.rooms());
                    self.healthy.store(false, Ordering::Release);
                }
                Err(error.into_io_error().to_string())
            }
        }
    }
}
