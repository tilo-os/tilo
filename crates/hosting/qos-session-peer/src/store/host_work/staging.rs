use super::*;
use crate::store::SessionStore;
use std::{
    ffi::CStr,
    fs::File,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::MetadataExt,
};

pub struct StagingDirectory {
    directory: File,
    key: DirectoryKey,
    inventory: HostWorkInventory,
}
pub struct StagingReservation {
    directory: StagingDirectory,
    identity: StageIdentity,
}
pub(super) fn io(_: std::io::Error) -> HostWorkFailure {
    HostWorkFailure::Unavailable
}
pub(super) fn private(file: &File, directory: bool) -> Result<(), HostWorkFailure> {
    let meta = file.metadata().map_err(io)?;
    if meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || if directory {
            !meta.is_dir()
        } else {
            !meta.is_file()
        }
    {
        return Err(HostWorkFailure::Unavailable);
    }
    Ok(())
}
pub(super) fn open_at(parent: &File, name: &CStr, flags: i32) -> Result<File, HostWorkFailure> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(HostWorkFailure::Unavailable);
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(super) fn visit(
    directory: &File,
    mut callback: impl FnMut(&CStr) -> Result<(), HostWorkFailure>,
) -> Result<(), HostWorkFailure> {
    let fd = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if fd < 0 {
        return Err(HostWorkFailure::Unavailable);
    }
    let raw = unsafe { libc::fdopendir(fd) };
    if raw.is_null() {
        unsafe { libc::close(fd) };
        return Err(HostWorkFailure::Unavailable);
    }
    struct Dir(*mut libc::DIR);
    impl Drop for Dir {
        fn drop(&mut self) {
            unsafe { libc::closedir(self.0) };
        }
    }
    let dir = Dir(raw);
    unsafe { libc::rewinddir(dir.0) };
    loop {
        #[cfg(target_os = "macos")]
        let errno = unsafe { libc::__error() };
        #[cfg(target_os = "linux")]
        let errno = unsafe { libc::__errno_location() };
        unsafe { *errno = 0 };
        let entry = unsafe { libc::readdir(dir.0) };
        if entry.is_null() {
            return if unsafe { *errno } == 0 {
                Ok(())
            } else {
                Err(HostWorkFailure::Unavailable)
            };
        }
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        callback(name)?;
    }
}
pub(super) fn disk_preflight(directory: &File, bytes: u64) -> Result<(), HostWorkFailure> {
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), stats.as_mut_ptr()) } != 0 {
        return Err(HostWorkFailure::Unavailable);
    }
    let stats = unsafe { stats.assume_init() };
    let fragment = stats.f_frsize as u64;
    if fragment == 0 {
        return Err(HostWorkFailure::Unavailable);
    }
    let available = (stats.f_bavail as u64)
        .checked_mul(fragment)
        .ok_or(HostWorkFailure::Unavailable)?;
    let total = (stats.f_blocks as u64)
        .checked_mul(fragment)
        .ok_or(HostWorkFailure::Unavailable)?;
    crate::store::check_space(
        available,
        total,
        stats.f_bsize as u64,
        bytes.try_into().map_err(|_| HostWorkFailure::Capacity)?,
    )
    .map_err(|e| match e {
        crate::store::StorePreflightFailure::Capacity => HostWorkFailure::Capacity,
        _ => HostWorkFailure::Unavailable,
    })
}
impl HostWorkInventory {
    /// Open staging relative to the caller's actual admitted directory handle.
    /// The caller retains its original namespace/owner lock. This lends no
    /// state authority and never reopens an ambient root path.
    pub fn staging_directory(
        &self,
        root: &File,
        workspace: u64,
    ) -> Result<StagingDirectory, HostWorkFailure> {
        if workspace == 0 {
            return Err(HostWorkFailure::Unavailable);
        }
        private(root, true)?;
        let meta = root.metadata().map_err(io)?;
        let key = DirectoryKey {
            device: meta.dev(),
            inode: meta.ino(),
            workspace,
        };
        let created =
            unsafe { libc::mkdirat(root.as_raw_fd(), c"staging-v1".as_ptr(), 0o700) } == 0;
        if !created && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
            return Err(HostWorkFailure::Unavailable);
        }
        let directory = open_at(&root, c"staging-v1", libc::O_RDONLY | libc::O_DIRECTORY)?;
        private(&directory, true)?;
        // A previous mkdir may have succeeded before its parent sync failed.
        // Existing visibility is not evidence that this entry is durable.
        files::directory_barrier(root)?;
        let snapshot_directory = super::snapshot::open_root(root)?;
        let result = StagingDirectory {
            directory,
            key,
            inventory: self.clone(),
        };
        let mut state = self
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?;
        let existing = state.slots.iter().position(|s| s.key == Some(key));
        if existing.is_some_and(|i| state.slots[i].has_live_leases()) {
            // Live workers retain registered full totals. Their private files
            // cannot be truncated or reclaimed by a concurrent reopen scan.
            return Ok(result);
        }
        let previous = existing
            .map(|i| state.slots[i].reserved_bytes())
            .transpose()?
            .unwrap_or(0);
        let _scan_credit = self
            .inner()
            .pool
            .try_acquire_bulk(
                64 * 1024
                    + size_of::<[Option<StageRecord>; STAGE_SLOTS]>() * 2
                    + size_of::<[Option<SnapshotRecord>; SNAPSHOT_SLOTS]>() * 2,
            )
            .ok_or(HostWorkFailure::Capacity)?;
        let snapshots = super::snapshot::recover(&snapshot_directory)?;
        let recovered = files::recover(&result.directory, workspace)?;
        let total = recovered
            .iter()
            .flatten()
            .try_fold(
                snapshots.iter().flatten().count() as u64 * SNAPSHOT_BYTES,
                |sum, s| sum.checked_add(s.identity.total),
            )
            .ok_or(HostWorkFailure::Capacity)?;
        let disk = state
            .disk
            .checked_sub(previous)
            .and_then(|n| n.checked_add(total))
            .ok_or(HostWorkFailure::Capacity)?;
        if disk > HOST_STAGE_BYTES || total > WORKSPACE_STAGE_BYTES {
            return Err(HostWorkFailure::Capacity);
        }
        let index = existing
            .or_else(|| {
                state
                    .slots
                    .iter()
                    .position(|s| s.key.is_none() || s.empty_records())
            })
            .ok_or(HostWorkFailure::Capacity)?;
        state.slots[index].key = Some(key);
        state.slots[index].stages = recovered;
        state.slots[index].snapshots = snapshots;
        state.disk = disk;
        Ok(result)
    }
}
impl SessionStore {
    pub(crate) fn staging_directory(
        &self,
        inventory: &HostWorkInventory,
        workspace: u64,
    ) -> Result<StagingDirectory, HostWorkFailure> {
        if self.is_poisoned() {
            return Err(HostWorkFailure::Unavailable);
        }
        inventory.staging_directory(&self.directory, workspace)
    }
    pub(crate) fn reserve_staging(
        &self,
        inventory: &HostWorkInventory,
        identity: StageIdentity,
    ) -> Result<StagingReservation, HostWorkFailure> {
        self.staging_directory(inventory, identity.workspace)?
            .reserve(identity)
    }
}
impl StagingDirectory {
    #[cfg(test)]
    pub(crate) fn with_barrier_fault<T>(&self, callback: impl FnOnce() -> T) -> T {
        files::barrier_fault::with(&self.directory, callback)
    }
    /// Stage identities of one device with no live lease (no worker, file or
    /// idle handle holds them), for the owner's scoped reclamation of expired
    /// or already admitted staging. Listing grants no custody or authority.
    pub(crate) fn idle_stages(
        &self,
        device: [u8; 32],
    ) -> Result<Vec<StageIdentity>, HostWorkFailure> {
        let state = self
            .inventory
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?;
        let slot = state
            .slots
            .iter()
            .find(|slot| slot.key == Some(self.key))
            .ok_or(HostWorkFailure::Unavailable)?;
        Ok(slot
            .stages
            .iter()
            .flatten()
            .filter(|record| record.identity.device == device && record.leases == 0)
            .map(|record| record.identity)
            .collect())
    }
    pub fn reserve(&self, identity: StageIdentity) -> Result<StagingReservation, HostWorkFailure> {
        if identity.workspace != self.key.workspace {
            return Err(HostWorkFailure::Conflict);
        }
        if identity.device == [0; 32]
            || identity.delivery == [0; 16]
            || identity.digest == [0; 32]
            || identity.total == 0
            || identity.total > 64 * 1024 * 1024
        {
            return Err(HostWorkFailure::Capacity);
        }
        let directory = Self {
            directory: self.directory.try_clone().map_err(io)?,
            key: self.key,
            inventory: self.inventory.clone(),
        };
        let mut state = self
            .inventory
            .inner()
            .state
            .lock()
            .map_err(|_| HostWorkFailure::Unavailable)?;
        let index = state
            .slots
            .iter()
            .position(|s| s.key == Some(self.key))
            .ok_or(HostWorkFailure::Unavailable)?;
        if let Some(prior) = state.slots[index].stages.iter_mut().flatten().find(|s| {
            s.identity.device == identity.device && s.identity.delivery == identity.delivery
        }) {
            if prior.identity != identity {
                return Err(HostWorkFailure::Conflict);
            }
            prior.leases = prior
                .leases
                .checked_add(1)
                .ok_or(HostWorkFailure::Capacity)?;
            return Ok(StagingReservation {
                directory,
                identity,
            });
        }
        let slot = &state.slots[index];
        if slot
            .stages
            .iter()
            .flatten()
            .filter(|s| s.identity.device == identity.device)
            .count()
            >= 2
        {
            return Err(HostWorkFailure::Capacity);
        }
        let position = slot
            .stages
            .iter()
            .position(Option::is_none)
            .ok_or(HostWorkFailure::Capacity)?;
        let workspace = slot
            .reserved_bytes()?
            .checked_add(identity.total)
            .ok_or(HostWorkFailure::Capacity)?;
        let host = state
            .disk
            .checked_add(identity.total)
            .ok_or(HostWorkFailure::Capacity)?;
        if workspace > WORKSPACE_STAGE_BYTES || host > HOST_STAGE_BYTES {
            return Err(HostWorkFailure::Capacity);
        }
        disk_preflight(&self.directory, host)?;
        state.disk = host;
        state.slots[index].stages[position] = Some(StageRecord {
            identity,
            leases: 1,
        });
        Ok(StagingReservation {
            directory,
            identity,
        })
    }
}
impl Drop for StagingReservation {
    fn drop(&mut self) {
        let mut state = self
            .directory
            .inventory
            .inner()
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(record) = state
            .slots
            .iter_mut()
            .find(|s| s.key == Some(self.directory.key))
            .and_then(|s| {
                s.stages
                    .iter_mut()
                    .flatten()
                    .find(|r| r.identity == self.identity)
            })
        {
            record.leases = record.leases.saturating_sub(1);
        }
        // The disk reservation remains until verified file cleanup and barrier.
    }
}

#[path = "staging/files.rs"]
pub(super) mod files;
