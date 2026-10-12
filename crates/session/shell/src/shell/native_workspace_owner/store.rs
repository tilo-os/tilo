//! One state-file writer shared by UI snapshots and committed native owners.
use super::{NativeAuthorityStorage, NativeWorkspaceOwner};
use crate::shell::state::{WeakWorkspaceTreeHandle, WorkspaceTreeHandle};
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

/// A file written from a snapshot of the tree, beside the state file. The
/// tree document is not one: it is written as a log of changes, in the
/// order they were made (see [`NativeStateRuntime::document_log`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Companion {
    /// The capability list the filesystem enforces.
    FsCapabilities = 0,
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
/// Policy observers must not retain the state writer or its exclusive file lease.
#[derive(Clone)]
pub(crate) struct NativeStateWitness(Weak<Runtime>);
/// Static rejection categories contain no peer, capability, path or process data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AuthorityRejection {
    WriterRetired,
    WriterFenced,
    RoomUncertain,
    InvalidSnapshot,
    StaleAcceptedSnapshot,
    RoomOwnerRetired,
    RoomOwnerNotCurrent,
}
impl AuthorityRejection {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::WriterRetired => "writer_retired",
            Self::WriterFenced => "writer_fenced",
            Self::RoomUncertain => "room_uncertain",
            Self::InvalidSnapshot => "invalid_snapshot",
            Self::StaleAcceptedSnapshot => "stale_accepted_snapshot",
            Self::RoomOwnerRetired => "room_owner_retired",
            Self::RoomOwnerNotCurrent => "room_owner_not_current",
        }
    }
}
impl NativeStateWitness {
    /// Borrow the protected ordinary-room writer for one host operation.
    /// Enrolled rooms must instead borrow their committed native view.
    pub(crate) fn with_current_ordinary<T>(
        &self,
        storage: &NativeAuthorityStorage,
        observed: &NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let runtime = self
            .0
            .upgrade()
            .ok_or("The accepted desktop writer has retired.")?;
        if storage.requires(room)
            || !runtime.healthy.load(Ordering::Acquire)
            || runtime.uncertain_rooms.read().contains(&room)
        {
            return Err("The ordinary desktop policy is not current.".into());
        }
        let expected = digest(storage).map_err(|error| error.to_string())?;
        if digest(observed).map_err(|error| error.to_string())? != expected {
            return Err("The accepted desktop authority has changed.".into());
        }
        let writer = runtime.writer.lock();
        if writer.expected != Some(expected)
            || writer.lease.is_none()
            || writer
                .owner
                .as_ref()
                .is_some_and(|(owner, _)| owner.upgrade().is_none())
        {
            return Err("The accepted desktop writer is unavailable or stale.".into());
        }
        let result = operation();
        drop(writer);
        drop(runtime);
        result
    }

    #[cfg(test)]
    pub(crate) fn retained_owners(&self) -> usize {
        self.0.strong_count()
    }
    pub(crate) fn authority_is_current(
        &self,
        storage: &NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
    ) -> bool {
        self.authority_status(storage, room).is_ok()
    }
    pub(crate) fn authority_status(
        &self,
        storage: &NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
    ) -> Result<(), AuthorityRejection> {
        let runtime = self.0.upgrade().ok_or(AuthorityRejection::WriterRetired)?;
        NativeStateRuntime(runtime).authority_status(storage, room)
    }
}
impl std::fmt::Debug for NativeStateRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeStateRuntime")
    }
}
pub(super) struct Runtime {
    pub writer: Mutex<Writer>,
    pub(super) local_filesystem_consents: Mutex<super::local_filesystem_consents::Consents>,
    pub healthy: AtomicBool,
    uncertain_rooms: RwLock<std::collections::BTreeSet<crate::shell::state::RoomId>>,
    /// Hands out [`StateSequence`]s; see [`NativeStateRuntime::next_sequence`].
    sequence: AtomicU64,
    /// Per [`Companion`], the newest snapshot it was written from. Held
    /// across the write, so a companion file never goes backwards.
    companions: Mutex<[u64; 1]>,
    /// The tree document beside the state file.
    document: crate::shell::persist::DocLog,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            writer: Mutex::new(Writer::default()),
            local_filesystem_consents: Mutex::default(),
            healthy: AtomicBool::new(true),
            uncertain_rooms: RwLock::new(std::collections::BTreeSet::default()),
            sequence: AtomicU64::new(0),
            companions: Mutex::new([0; 1]),
            document: crate::shell::persist::DocLog::default(),
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
        WeakWorkspaceTreeHandle,
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
    pub(crate) fn authority_witness(&self) -> NativeStateWitness {
        NativeStateWitness(Arc::downgrade(&self.0))
    }
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
        if !bound.ptr_eq(&tree.downgrade()) {
            return Err("state writer already belongs to a different native tree handle".into());
        }
        Ok(Some(owner))
    }
    pub(super) fn attach(&self, owner: &NativeWorkspaceOwner, tree: &WorkspaceTreeHandle) {
        self.0.writer.lock().owner = Some((owner.downgrade(), tree.downgrade()));
    }
    pub(crate) fn requires_uncertain(&self, room: crate::shell::state::RoomId) -> bool {
        self.0.uncertain_rooms.read().contains(&room)
    }
    /// Read-only witness for host policy exports. A stale/fenced native
    /// authority must not renew an application action lease.
    fn authority_status(
        &self,
        storage: &NativeAuthorityStorage,
        room: crate::shell::state::RoomId,
    ) -> Result<(), AuthorityRejection> {
        if !self.healthy() {
            return Err(AuthorityRejection::WriterFenced);
        }
        if self.requires_uncertain(room) {
            return Err(AuthorityRejection::RoomUncertain);
        }
        let expected = digest(storage).map_err(|_| AuthorityRejection::InvalidSnapshot)?;
        let current = self.0.writer.lock().expected;
        if current.is_some_and(|current| current != expected) {
            return Err(AuthorityRejection::StaleAcceptedSnapshot);
        }
        if storage.requires(room) {
            let owner = self.owner().ok_or(AuthorityRejection::RoomOwnerRetired)?;
            return owner
                .with_current(room, |_| Ok(()))
                .map_err(|_| AuthorityRejection::RoomOwnerNotCurrent);
        }
        Ok(())
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

    /// The tree document beside this state file: every copy of the tree
    /// cuts its changes into it, and every save writes them.
    pub(crate) fn document_log(&self) -> &crate::shell::persist::DocLog {
        &self.0.document
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

#[cfg(test)]
mod action_authority_tests {
    use super::*;
    #[test]
    fn action_authority_witness_rejects_fenced_uncertain_and_stale_native_writer() {
        let runtime = NativeStateRuntime::default();
        let witness = runtime.authority_witness();
        let storage = NativeAuthorityStorage::default();
        let room =
            crate::shell::state::RoomId::parse("00000000-0000-4000-8000-000000000123").unwrap();
        assert!(witness.authority_is_current(&storage, room));
        runtime.0.healthy.store(false, Ordering::Release);
        assert!(!witness.authority_is_current(&storage, room));
        runtime.0.healthy.store(true, Ordering::Release);
        runtime.0.uncertain_rooms.write().insert(room);
        assert!(!witness.authority_is_current(&storage, room));
        runtime.0.uncertain_rooms.write().remove(&room);
        runtime.0.writer.lock().expected = Some([0xff; 32]);
        assert!(!witness.authority_is_current(&storage, room));
        runtime.0.writer.lock().expected = Some(digest(&storage).unwrap());
        assert!(witness.authority_is_current(&storage, room));
        drop(runtime);
        assert!(!witness.authority_is_current(&storage, room));
    }
}
