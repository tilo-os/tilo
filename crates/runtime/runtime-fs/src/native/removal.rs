//! Host-bound private same-mount removal ownership. No pathname constructs it.
use super::{
    Arc, Dir, DirectoryGrant, EntryName, FsError, FsRight, FsSession, GrantLease, MetadataExt,
    NativeIdentity, NativeLocator, OFlags, io_error, lock,
};
#[cfg(target_os = "linux")]
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::MetadataExt as StdMetadataExt;

pub(crate) struct RemovalOwnership {
    pub directory: Arc<Dir>,
    pub guard: std::fs::File,
    pub root: GrantLease,
    pub identity: NativeIdentity,
    guard_identity: NativeIdentity,
    pub(super) owner_key: std::sync::Mutex<Option<[u8; 32]>>,
}
fn host_identity(metadata: &std::fs::Metadata) -> NativeIdentity {
    NativeIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}
#[cfg(target_os = "linux")]
fn mount(object: std::os::fd::BorrowedFd<'_>) -> Result<u64, FsError> {
    let facts = rustix::fs::statx(
        object,
        c"",
        rustix::fs::AtFlags::EMPTY_PATH | rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        rustix::fs::StatxFlags::MNT_ID,
    )
    .map_err(|error| io_error(error.into()))?;
    if facts.stx_mask & rustix::fs::StatxFlags::MNT_ID.bits() == 0 {
        return Err(FsError::Unsupported("native mount identity unavailable"));
    }
    Ok(facts.stx_mnt_id)
}
fn parent(directory: &Dir) -> Result<std::fs::File, FsError> {
    #[cfg(target_os = "linux")]
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(directory, "..", flags, rustix::fs::Mode::empty())
        .map(std::fs::File::from)
        .map_err(|e| io_error(e.into()))
}
impl RemovalOwnership {
    pub(crate) fn check(&self) -> Result<(), FsError> {
        let held = self.directory.dir_metadata().map_err(io_error)?;
        let actual_guard = parent(&self.directory)?;
        let guard = actual_guard.metadata().map_err(io_error)?;
        if NativeIdentity::of(&held) != self.identity
            || host_identity(&self.guard.metadata().map_err(io_error)?) != self.guard_identity
            || host_identity(&guard) != self.guard_identity
            || guard.uid() != 0
            || guard.mode() & 0o7777 != 0o700
            || held.uid() != rustix::process::geteuid().as_raw()
            || held.mode() & 0o7777 != 0o700
        {
            return Err(FsError::StaleEntry);
        }
        Ok(())
    }
}
impl FsSession {
    /// Bind a descriptor delegated by the authenticated host below a root-only
    /// guard. A user-owned private directory cannot substitute for this ownership.
    /// # Errors
    /// Refuses absent live Enumerate, wrong ancestry/UID/mode, another VFS
    /// mount or rebinding. Binding ownership itself admits no mutation.
    pub fn bind_removal_directory(
        &self,
        root: &DirectoryGrant,
        opened: std::fs::File,
    ) -> Result<(), FsError> {
        let table = self.call()?;
        self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
        let directory = Arc::new(Dir::from_std_file(opened));
        let guard = parent(&directory)?;
        let identity = NativeIdentity::of(&directory.dir_metadata().map_err(io_error)?);
        let guard_identity = host_identity(&guard.metadata().map_err(io_error)?);
        let source = Self::directory(&root.lease)?;
        if identity.device != source.dir_metadata().map_err(io_error)?.dev() {
            return Err(FsError::Unsupported(
                "removal ownership must share source filesystem",
            ));
        }
        #[cfg(target_os = "linux")]
        if mount(directory.as_fd())? != mount(source.as_fd())? {
            return Err(FsError::Unsupported(
                "removal ownership must share source VFS mount",
            ));
        }
        let ownership = Arc::new(RemovalOwnership {
            directory,
            guard,
            root: root.lease.clone(),
            identity,
            guard_identity,
            owner_key: std::sync::Mutex::new(None),
        });
        ownership.check()?;
        let mut slot = lock(&self.inner.removal)?;
        if slot.is_some() {
            return Err(FsError::Busy);
        }
        *slot = Some(ownership);
        drop(slot);
        drop(table);
        Ok(())
    }
    /// Bind only the immutable root-created derivative of the independently
    /// authenticated original source record. Byte arrays cannot admit owners.
    #[cfg(target_os = "linux")]
    pub fn bind_retained_source_owner(
        &self,
        root: &DirectoryGrant,
        mut sealed: std::fs::File,
    ) -> Result<(), FsError> {
        use std::io::{Read, Seek};
        let table = self.call()?;
        self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
        let ownership = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        if !Arc::ptr_eq(&root.lease.record, &ownership.root.record) {
            return Err(FsError::Denied);
        }
        ownership.check()?;
        let metadata = sealed.metadata().map_err(io_error)?;
        let seals = unsafe { libc::fcntl(sealed.as_raw_fd(), libc::F_GET_SEALS) };
        let required =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        if metadata.uid() != 0
            || !metadata.is_file()
            || metadata.nlink() != 0
            || metadata.len() != 32
            || seals < 0
            || seals & required != required
        {
            return Err(FsError::Denied);
        }
        sealed.rewind().map_err(io_error)?;
        let mut key = [0; 32];
        sealed.read_exact(&mut key).map_err(io_error)?;
        if key == [0; 32] {
            return Err(FsError::Denied);
        }
        let mut slot = lock(&ownership.owner_key)?;
        if slot.is_some() {
            return Err(FsError::Busy);
        }
        *slot = Some(key);
        drop(slot);
        drop(table);
        Ok(())
    }
    /// Bind retained source authority only on supported hosts.
    #[cfg(not(target_os = "linux"))]
    pub fn bind_retained_source_owner(
        &self,
        _root: &DirectoryGrant,
        _sealed: std::fs::File,
    ) -> Result<(), FsError> {
        Err(FsError::Unsupported(
            "root-sealed retained source requires Linux",
        ))
    }
    /// Metadata identifier of the actual admitted source, never an admission.
    pub fn retained_source_owner_key(&self) -> Result<[u8; 32], FsError> {
        let table = self.call()?;
        let ownership = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        self.validate(&table, &ownership.root, Some(FsRight::Enumerate))?;
        ownership.check()?;
        let key = lock(&ownership.owner_key)?
            .as_ref()
            .copied()
            .ok_or(FsError::Denied)?;
        Ok(key)
    }
    pub(crate) fn retained_journal_roots(
        &self,
        root: &DirectoryGrant,
        journal: &std::fs::File,
    ) -> Result<(NativeIdentity, NativeIdentity), FsError> {
        let table = self.call()?;
        self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
        let ownership = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        if !Arc::ptr_eq(&root.lease.record, &ownership.root.record) {
            return Err(FsError::Denied);
        }
        ownership.check()?;
        let actual = journal.metadata().map_err(io_error)?;
        if !actual.is_dir()
            || actual.uid() != rustix::process::geteuid().as_raw()
            || actual.mode() & 0o7777 != 0o700
        {
            return Err(FsError::Denied);
        }
        let copy = Dir::from_std_file(journal.try_clone().map_err(io_error)?);
        let guard = parent(&copy)?;
        if host_identity(&guard.metadata().map_err(io_error)?) != ownership.guard_identity {
            return Err(FsError::Denied);
        }
        let home = root.lease.object()?.identity();
        if actual.dev() != home.device {
            return Err(FsError::Denied);
        }
        #[cfg(target_os = "linux")]
        if mount(journal.as_fd())? != mount(Self::directory(&root.lease)?.as_fd())? {
            return Err(FsError::Denied);
        }
        Ok((home, ownership.identity))
    }
    pub(crate) fn recovery_scope(&self, parent: &DirectoryGrant) -> Result<NativeLocator, FsError> {
        let table = self.call()?;
        self.validate(&table, &parent.lease, Some(FsRight::Enumerate))?;
        let ownership = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        self.validate(&table, &ownership.root, Some(FsRight::Enumerate))?;
        if !parent.lease.descends_from(&ownership.root) {
            return Err(FsError::Denied);
        }
        ownership.check()?;
        let label = EntryName::new(std::ffi::OsString::from("recovery-scope"))?;
        self.locator_locked(&parent.lease, &label, None)
    }
    pub(crate) fn recovery_ownership(
        &self,
        parent: &DirectoryGrant,
        stage: &NativeLocator,
    ) -> Result<Arc<RemovalOwnership>, FsError> {
        let table = self.call()?;
        let ownership = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        self.validate(&table, &ownership.root, Some(FsRight::Enumerate))?;
        self.validate(&table, &parent.lease, Some(FsRight::Create))?;
        if !parent.lease.descends_from(&ownership.root)
            || stage.root != ownership.identity
            || !stage.parents.is_empty()
        {
            return Err(FsError::Denied);
        }
        ownership.check()?;
        Ok(ownership)
    }
    pub(crate) fn captured_restore_effect<T>(
        &self,
        ownership: &RemovalOwnership,
        parent: &DirectoryGrant,
        action: impl FnOnce() -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        let table = self.call()?;
        self.validate(&table, &ownership.root, Some(FsRight::Enumerate))?;
        self.validate(&table, &parent.lease, Some(FsRight::Create))?;
        if !parent.lease.descends_from(&ownership.root) {
            return Err(FsError::Denied);
        }
        ownership.check()?;
        action()
    }
}
