//! One state-file writer shared by UI snapshots and committed native owners.
use super::{SharedOwner, record::NativeAuthorityStorage};
use atomic_file::lease::{FileLease, LeaseMode};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

pub(crate) const MAX_AUTHORITY_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_STATE_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_PENDING: usize = 8;

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
    uncertain_rooms: RwLock<std::collections::BTreeSet<super::super::state::RoomId>>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            writer: Mutex::new(Writer::default()),
            healthy: AtomicBool::new(true),
            uncertain_rooms: RwLock::new(Default::default()),
        }
    }
}
#[derive(Default)]
pub(super) struct Writer {
    path: Option<PathBuf>,
    lease: Option<FileLease>,
    expected: Option<[u8; 32]>,
    pub owner: Weak<SharedOwner>,
}

pub(crate) struct LimitedWriter<W> {
    inner: W,
    remaining: usize,
}
impl<W> LimitedWriter<W> {
    pub(crate) fn new(inner: W, limit: usize) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
    pub(crate) fn into_inner(self) -> W {
        self.inner
    }
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other(
                "native encoded state refusal cap exceeded",
            ));
        }
        let n = self.inner.write(bytes)?;
        self.remaining -= n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
pub(crate) fn preflight<T: Serialize>(value: &T, limit: usize) -> io::Result<()> {
    serde_json::to_writer_pretty(LimitedWriter::new(io::sink(), limit), value)
        .map_err(io::Error::other)
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
    super::publication::preflight_storage(storage).map_err(io::Error::other)?;
    let mut hash = HashWriter(blake3::Hasher::new());
    serde_json::to_writer(&mut hash, storage).map_err(io::Error::other)?;
    Ok(*hash.0.finalize().as_bytes())
}
impl NativeStateRuntime {
    pub(crate) fn owner(&self) -> Option<super::NativeWorkspaceOwner> {
        self.0
            .writer
            .lock()
            .owner
            .upgrade()
            .map(super::NativeWorkspaceOwner)
    }
    pub(crate) fn requires_uncertain(&self, room: super::super::state::RoomId) -> bool {
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
        match atomic_file::replace(path, bytes, atomic_file::Privacy::OwnerOnly) {
            Ok(()) => {
                writer.expected = Some(next_digest);
                Ok(())
            }
            Err(error) => {
                if error.state() != atomic_file::PublicationState::NotPublished {
                    {
                        let mut uncertain = self.0.uncertain_rooms.write();
                        uncertain.extend(next.rooms.keys().copied());
                        uncertain.extend(next.pending.keys().copied());
                    }
                    self.0.healthy.store(false, Ordering::Release);
                    let owner = writer.owner.upgrade();
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
            .unwrap_or(Path::new("."));
        atomic_file::provision_directory(parent, atomic_file::Privacy::Inherit)
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
            file.take(MAX_STATE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_STATE_BYTES {
                return Err(io::Error::other(
                    "native encoded state refusal cap exceeded",
                ));
            }
            let disk: DiskAuthority = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if disk.schema_version > super::super::persist::NATIVE_STATE_SCHEMA_VERSION
                || (disk.schema_version < 2 && !disk.native_authority.is_empty())
            {
                return Err(io::Error::other(
                    "unsupported native authority state-file layout",
                ));
            }
            super::publication::validate_file_layout(disk.schema_version, &disk.native_authority)
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
