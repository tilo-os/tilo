//! One state-file writer shared by UI snapshots and committed native owners.
use super::{NativeAuthorityStorage, NativeWorkspaceOwner};
use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
use parking_lot::{Mutex, RwLock};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tilo_files::native::atomic::lease::{FileLease, LeaseMode};
pub(crate) use tilo_joined_room::{LimitedWriter, MAX_AUTHORITY_BYTES, MAX_STATE_BYTES, preflight};

/// Where a snapshot stands among the writes of one state file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct StateSequence(u64);

/// A file written from a snapshot of the tree, beside the state file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Companion {
    /// The tree document and its meta.
    Document = 0,
    /// The capability list the filesystem enforces.
    FsCapabilities = 1,
}

/// What [`NativeStateRuntime::publish`] did with a snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Publication {
    Written,
    /// A later snapshot is already in the file; this one was dropped.
    Superseded,
}

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
    /// Hands out [`StateSequence`]s; see [`NativeStateRuntime::next_sequence`].
    sequence: AtomicU64,
    /// Per [`Companion`], the newest snapshot it was written from. Held
    /// across the write, so a companion file never goes backwards.
    companions: Mutex<[u64; 2]>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            writer: Mutex::new(Writer::default()),
            healthy: AtomicBool::new(true),
            uncertain_rooms: RwLock::new(std::collections::BTreeSet::default()),
            sequence: AtomicU64::new(0),
            companions: Mutex::new([0; 2]),
        }
    }
}
#[derive(Default)]
pub(super) struct Writer {
    path: Option<PathBuf>,
    lease: Option<FileLease>,
    expected: Option<[u8; 32]>,
    /// The newest snapshot published to the state file.
    published: u64,
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
    /// Order of a snapshot of the tree among every other snapshot of it.
    /// Taken while the caller holds the tree, so a later sequence is never an
    /// older state. A write carrying it is skipped once a later one landed:
    /// background saves and synchronous ones share the files.
    pub(crate) fn next_sequence(&self) -> StateSequence {
        StateSequence(
            self.0
                .sequence
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1),
        )
    }

    /// Run `write` for `companion` of snapshot `sequence`, unless that file
    /// was already written from a later snapshot.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the lock is held across the write so companions are written in order"
    )]
    pub(crate) fn write_companion(
        &self,
        companion: Companion,
        sequence: StateSequence,
        write: impl FnOnce(),
    ) {
        let mut companions = self.0.companions.lock();
        let Some(written) = companions.get_mut(companion as usize) else {
            return;
        };
        if sequence.0 < *written {
            return;
        }
        write();
        *written = sequence.0;
    }

    pub(crate) fn publish(
        &self,
        path: &Path,
        base: &NativeAuthorityStorage,
        next: &NativeAuthorityStorage,
        bytes: &[u8],
        sequence: StateSequence,
    ) -> io::Result<Publication> {
        let base_digest = digest(base)?;
        let next_digest = digest(next)?;
        let mut writer = self.0.writer.lock();
        if sequence.0 < writer.published {
            return Ok(Publication::Superseded);
        }
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
                writer.published = sequence.0;
                Ok(Publication::Written)
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
