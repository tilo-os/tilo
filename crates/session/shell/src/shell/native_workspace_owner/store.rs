//! One state-file writer shared by UI snapshots and committed native owners.
use super::{NativeAuthorityStorage, NativeWorkspaceOwner};
use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
use parking_lot::{Mutex, RwLock};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tilo_files::native::atomic::lease::{FileLease, LeaseMode};
pub(crate) use tilo_joined_room::{LimitedWriter, MAX_AUTHORITY_BYTES, MAX_STATE_BYTES, preflight};

/// Clones of the desktop share the writer, so stale UI snapshots cannot erase floors.
#[derive(Clone, Default)]
pub struct NativeStateRuntime(pub(super) Arc<Runtime>);
impl std::fmt::Debug for NativeStateRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeStateRuntime")
    }
}
pub(super) struct Runtime {
    pub writer: Mutex<Writer>,
    pub healthy: AtomicBool,
    uncertain_rooms: RwLock<std::collections::BTreeSet<crate::shell::state::RoomId>>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            writer: Mutex::new(Writer::default()),
            healthy: AtomicBool::new(true),
            uncertain_rooms: RwLock::new(std::collections::BTreeSet::default()),
        }
    }
}
#[derive(Default)]
pub(super) struct Writer {
    path: Option<PathBuf>,
    lease: Option<FileLease>,
    expected: Option<[u8; 32]>,
    /// The attached owner and the tree whose state file it publishes into.
    owner: Option<(
        tilo_joined_room::WeakNativeWorkspaceOwner,
        Weak<RwLock<WorkspaceTree>>,
    )>,
}

struct HashWriter(blake3::Hasher);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn digest(storage: &NativeAuthorityStorage) -> io::Result<[u8; 32]> {
    tilo_joined_room::preflight_storage(storage).map_err(io::Error::other)?;
    let mut hash = HashWriter(blake3::Hasher::new());
    serde_json::to_writer(&mut hash, storage).map_err(io::Error::other)?;
    Ok(*hash.0.finalize().as_bytes())
}
impl NativeStateRuntime {
    pub(crate) fn owner(&self) -> Option<NativeWorkspaceOwner> {
        self.0
            .writer
            .lock()
            .owner
            .as_ref()
            .and_then(|(owner, _)| owner.upgrade())
    }
    /// The owner already attached to this writer, if it belongs to `tree`.
    pub(super) fn owner_of(
        &self,
        tree: &WorkspaceTreeHandle,
    ) -> Result<Option<NativeWorkspaceOwner>, String> {
        let Some((owner, bound)) = self.0.writer.lock().owner.clone() else {
            return Ok(None);
        };
        let Some(owner) = owner.upgrade() else {
            return Ok(None);
        };
        if !Weak::ptr_eq(&bound, &Arc::downgrade(tree)) {
            return Err("state writer already belongs to a different native tree handle".into());
        }
        Ok(Some(owner))
    }
    pub(super) fn attach(&self, owner: &NativeWorkspaceOwner, tree: &WorkspaceTreeHandle) {
        self.0.writer.lock().owner = Some((owner.downgrade(), Arc::downgrade(tree)));
    }
    pub(crate) fn requires_uncertain(&self, room: crate::shell::state::RoomId) -> bool {
        self.0.uncertain_rooms.read().contains(&room)
    }
    pub(super) fn healthy(&self) -> bool {
        self.0.healthy.load(Ordering::Acquire)
    }
    /// Called only while the tree snapshot is held. `next` differs only for owner turns.
    pub(crate) fn publish(
        &self,
        path: &Path,
        base: &NativeAuthorityStorage,
        next: &NativeAuthorityStorage,
        bytes: &[u8],
    ) -> io::Result<()> {
        let base_digest = digest(base)?;
        let next_digest = digest(next)?;
        let mut writer = self.0.writer.lock();
        if !self.healthy() {
            return Err(io::Error::other(
                "native state writer is fenced after uncertain publication",
            ));
        }
        writer.bind(path, base_digest)?;
        if writer.expected != Some(base_digest) {
            return Err(io::Error::other("stale native authority snapshot refused"));
        }
        match tilo_files::native::atomic::replace(
            path,
            bytes,
            tilo_files::native::atomic::Privacy::OwnerOnly,
        ) {
            Ok(()) => {
                writer.expected = Some(next_digest);
                Ok(())
            }
            Err(error) => {
                if error.state() != tilo_files::native::atomic::PublicationState::NotPublished {
                    {
                        let mut uncertain = self.0.uncertain_rooms.write();
                        uncertain.extend(next.rooms());
                    }
                    self.0.healthy.store(false, Ordering::Release);
                    let owner = writer.owner.as_ref().and_then(|(owner, _)| owner.upgrade());
                    drop(writer);
                    if let Some(owner) = owner {
                        owner.fence_all();
                    }
                }
                Err(error.into_io_error())
            }
        }
    }
    pub(super) fn bind(&self, path: &Path, storage: &NativeAuthorityStorage) -> io::Result<()> {
        let expected = digest(storage)?;
        self.0.writer.lock().bind(path, expected)
    }
}
impl Writer {
    fn bind(&mut self, path: &Path, expected: [u8; 32]) -> io::Result<()> {
        if let Some(bound) = &self.path {
            if bound != path {
                return Err(io::Error::other(
                    "native writer cannot change state-file path",
                ));
            }
            return Ok(());
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        tilo_files::native::atomic::provision_directory(
            parent,
            tilo_files::native::atomic::Privacy::Inherit,
        )
        .map_err(io::Error::other)?;
        let mut sentinel = path.as_os_str().to_os_string();
        sentinel.push(".writer-lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(PathBuf::from(sentinel))?;
        let lease = FileLease::try_acquire(file, LeaseMode::Exclusive).map_err(io::Error::other)?;
        if path.exists() {
            #[derive(serde::Deserialize)]
            struct DiskAuthority {
                #[serde(default)]
                schema_version: u32,
                #[serde(default)]
                native_authority: NativeAuthorityStorage,
            }
            use std::io::Read;
            let file = std::fs::File::open(path)?;
            let mut bytes = Vec::new();
            file.take((MAX_STATE_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_STATE_BYTES {
                return Err(io::Error::other(
                    "native encoded state refusal cap exceeded",
                ));
            }
            let disk: DiskAuthority = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if disk.schema_version != crate::shell::persist::NATIVE_STATE_SCHEMA_VERSION {
                return Err(io::Error::other(
                    "unsupported native authority state-file layout",
                ));
            }
            tilo_joined_room::validate_file_layout(disk.schema_version, &disk.native_authority)
                .map_err(io::Error::other)?;
            if digest(&disk.native_authority)? != expected {
                return Err(io::Error::other(
                    "state-file authority changed since this snapshot was loaded",
                ));
            }
        }
        self.path = Some(path.into());
        self.expected = Some(expected);
        self.lease = Some(lease);
        Ok(())
    }
}
