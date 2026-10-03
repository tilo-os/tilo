//! The record in a file of its own, for a host that keeps no other state
//! beside it. The caller holds exclusive access to the file's directory.
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

/// Open the owner whose record is the file at `path`, starting empty.
///
/// # Errors
/// The file cannot be read or holds an invalid record.
pub fn open_file(path: &Path) -> Result<NativeWorkspaceOwner, String> {
    let authority = match std::fs::File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take((MAX_AUTHORITY_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > MAX_AUTHORITY_BYTES {
                return Err("joined-room record exceeds its cap".into());
            }
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            NativeAuthorityStorage::default()
        }
        Err(error) => return Err(error.to_string()),
    };
    let store = FileStore {
        path: path.to_path_buf(),
        healthy: AtomicBool::new(true),
        uncertain: RwLock::new(BTreeSet::new()),
    };
    NativeWorkspaceOwner::restore(Arc::new(store), authority, SystemTime::now())
}

struct FileStore {
    path: PathBuf,
    healthy: AtomicBool,
    uncertain: RwLock<BTreeSet<RoomId>>,
}

impl Store for FileStore {
    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn uncertain(&self, room: RoomId) -> bool {
        self.uncertain.read().contains(&room)
    }

    fn slot_room(&self, _slot: WorkspaceId) -> Option<RoomId> {
        None
    }

    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        _change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        if !self.healthy() {
            return Err("joined-room store is fenced after an uncertain publication".into());
        }
        preflight_storage(next)?;
        let mut bytes = LimitedWriter::new(Vec::new(), MAX_AUTHORITY_BYTES);
        serde_json::to_writer_pretty(&mut bytes, next).map_err(|e| e.to_string())?;
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
