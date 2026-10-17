//! Host-bound private same-mount removal custody. No pathname constructs it.
use super::{
    Arc, Dir, DirectoryGrant, EntryName, FsError, FsRight, FsSession, GrantLease, GrantedEntry,
    MetadataExt, NativeIdentity, NativeLocator, OFlags, io_error, lock,
};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt as StdMetadataExt;

type ReplacementIdentities = ([u64; 2], [u64; 2], [u64; 2]);

pub(crate) struct RemovalCustody {
    pub directory: Arc<Dir>,
    pub guard: std::fs::File,
    pub root: GrantLease,
    pub identity: NativeIdentity,
    guard_identity: NativeIdentity,
    pub(super) owner_key: std::sync::Mutex<Option<[u8; 32]>>,
    history_verifier: std::sync::Mutex<Option<crate::external_history::VerifierClaims>>,
}
fn host_identity(metadata: &std::fs::Metadata) -> NativeIdentity {
    NativeIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}
#[cfg(target_os = "linux")]
fn mount(fd: i32) -> Result<u64, FsError> {
    let mut value = std::mem::MaybeUninit::<libc::statx>::uninit();
    if unsafe {
        libc::statx(
            fd,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            value.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let value = unsafe { value.assume_init() };
    if value.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(FsError::Unsupported("native mount identity unavailable"));
    }
    Ok(value.stx_mnt_id)
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
impl RemovalCustody {
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
    /// guard. A user-owned private directory cannot substitute for this custody.
    /// # Errors
    /// Refuses absent live Enumerate, wrong ancestry/UID/mode, another VFS
    /// mount or rebinding. Binding custody itself admits no mutation.
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
                "removal custody must share source filesystem",
            ));
        }
        #[cfg(target_os = "linux")]
        if mount(directory.as_raw_fd())? != mount(source.as_raw_fd())? {
            return Err(FsError::Unsupported(
                "removal custody must share source VFS mount",
            ));
        }
        let custody = Arc::new(RemovalCustody {
            directory,
            guard,
            root: root.lease.clone(),
            identity,
            guard_identity,
            owner_key: std::sync::Mutex::new(None),
            history_verifier: std::sync::Mutex::new(None),
        });
        custody.check()?;
        let mut slot = lock(&self.inner.removal)?;
        if slot.is_some() {
            return Err(FsError::Busy);
        }
        *slot = Some(custody);
        drop(slot);
        drop(table);
        Ok(())
    }
    /// Bind only the immutable root-created derivative of the independently
    /// authenticated original source record. Byte arrays cannot admit owners.
    /// # Errors
    /// Refuses unsealed, non-root, malformed keys or unrelated live custody.
    #[cfg(target_os = "linux")]
    pub fn bind_retained_source_owner(
        &self,
        root: &DirectoryGrant,
        mut sealed: std::fs::File,
    ) -> Result<(), FsError> {
        use std::io::{Read, Seek};
        let table = self.call()?;
        self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        if !Arc::ptr_eq(&root.lease.record, &custody.root.record) {
            return Err(FsError::Denied);
        }
        custody.check()?;
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
        let mut slot = lock(&custody.owner_key)?;
        if slot.is_some() {
            return Err(FsError::Busy);
        }
        *slot = Some(key);
        drop(slot);
        drop(table);
        Ok(())
    }
    /// Bind retained source authority only on supported hosts.
    /// # Errors
    /// Always returns an unsupported-platform error outside Linux.
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
    /// # Errors
    /// Requires its live original root/custody and independently sealed key.
    pub fn retained_source_owner_key(&self) -> Result<[u8; 32], FsError> {
        let table = self.call()?;
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        self.validate(&table, &custody.root, Some(FsRight::Enumerate))?;
        custody.check()?;
        let key = lock(&custody.owner_key)?
            .as_ref()
            .copied()
            .ok_or(FsError::Denied)?;
        Ok(key)
    }
    /// Bind only the root-created sealed verifier admitted by the installed
    /// daemon and launcher for this exact original-source custody. This is not
    /// a public byte-array/key constructor and does not sign or complete history.
    /// # Errors
    /// Refuses wrong custody/owner, unsealed descriptors, malformed context and
    /// rebinding. Non-Linux workers cannot accept root-sealed source authority.
    #[cfg(target_os = "linux")]
    pub fn bind_retained_history_verifier(
        &self,
        root: &DirectoryGrant,
        mut sealed: std::fs::File,
    ) -> Result<(), FsError> {
        use std::io::{Read, Seek};
        let table = self.call()?;
        self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        if !Arc::ptr_eq(&root.lease.record, &custody.root.record) {
            return Err(FsError::Denied);
        }
        custody.check()?;
        let metadata = sealed.metadata().map_err(io_error)?;
        let seals = unsafe { libc::fcntl(sealed.as_raw_fd(), libc::F_GET_SEALS) };
        let required =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        if metadata.uid() != 0
            || !metadata.is_file()
            || metadata.nlink() != 0
            || metadata.len() == 0
            || metadata.len() > 4096
            || seals < 0
            || seals & required != required
        {
            return Err(FsError::Denied);
        }
        sealed.rewind().map_err(io_error)?;
        let mut bytes = Vec::new();
        sealed
            .take(4097)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        let verifier =
            crate::external_history::VerifierClaims::decode(&bytes).map_err(|_| FsError::Denied)?;
        if *lock(&custody.owner_key)? != Some(verifier.source_owner) {
            return Err(FsError::Denied);
        }
        custody.check()?;
        self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
        let mut slot = lock(&custody.history_verifier)?;
        if slot.is_some() {
            return Err(FsError::Busy);
        }
        *slot = Some(verifier);
        drop(slot);
        drop(table);
        Ok(())
    }
    /// Bind retained history authority only on supported hosts.
    /// # Errors
    /// Always returns an unsupported-platform error outside Linux.
    #[cfg(not(target_os = "linux"))]
    pub fn bind_retained_history_verifier(
        &self,
        _root: &DirectoryGrant,
        _sealed: std::fs::File,
    ) -> Result<(), FsError> {
        Err(FsError::Unsupported(
            "root-sealed retained history requires Linux",
        ))
    }
    /// Private live-custody observation; caller text cannot construct a verifier.
    pub(crate) fn retained_history_verifier(
        &self,
    ) -> Result<crate::external_history::VerifierClaims, FsError> {
        let table = self.call()?;
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        self.validate(&table, &custody.root, Some(FsRight::Enumerate))?;
        custody.check()?;
        let verifier = lock(&custody.history_verifier)?
            .clone()
            .ok_or(FsError::Denied)?;
        if *lock(&custody.owner_key)? != Some(verifier.source_owner) {
            return Err(FsError::Denied);
        }
        Ok(verifier)
    }
    /// Keep the existing exact native grants and revocation gate through signed
    /// journal metadata publication. This cannot create Replace authority.
    pub(crate) fn with_retained_replacement_scope<T>(
        &self,
        original: &GrantedEntry,
        victim: &GrantedEntry,
        parent: &DirectoryGrant,
        source_owner: [u8; 32],
        effect: impl FnOnce() -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        self.replacement_effect(&custody, original, victim, parent, || {
            if *lock(&custody.owner_key)? != Some(source_owner) {
                return Err(FsError::Denied);
            }
            effect()
        })
    }
    pub(crate) fn replacement_effect<T>(
        &self,
        custody: &RemovalCustody,
        original: &GrantedEntry,
        victim: &GrantedEntry,
        parent: &DirectoryGrant,
        effect: impl FnOnce() -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        let table = self.call()?;
        self.validate(&table, &custody.root, Some(FsRight::Replace))?;
        self.validate(&table, original.lease(), Some(FsRight::Rename))?;
        self.validate(&table, original.lease(), Some(FsRight::Write))?;
        self.validate(&table, victim.lease(), Some(FsRight::Replace))?;
        self.validate(&table, &parent.lease, Some(FsRight::Create))?;
        self.validate(&table, &parent.lease, Some(FsRight::Replace))?;
        if !original.lease().descends_from(&custody.root)
            || !victim.lease().descends_from(&custody.root)
            || !parent.lease.descends_from(&custody.root)
        {
            return Err(FsError::Denied);
        }
        custody.check()?;
        effect()
    }
    pub(crate) fn observe_replacement(
        &self,
        parent: &DirectoryGrant,
        step: &crate::recovery::Step,
    ) -> Result<ReplacementIdentities, FsError> {
        self.with_replacement_observation(parent, step, None, Ok)
    }
    pub(crate) fn with_replacement_observation<T>(
        &self,
        parent: &DirectoryGrant,
        step: &crate::recovery::Step,
        owner_key: Option<[u8; 32]>,
        effect: impl FnOnce(ReplacementIdentities) -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        let source = step
            .source
            .as_ref()
            .and_then(|value| value.identity)
            .ok_or(FsError::Denied)?;
        let stage = step.stage.as_ref().ok_or(FsError::Denied)?;
        let destination = step.destination.as_ref().ok_or(FsError::Denied)?;
        let name = EntryName::new(std::ffi::OsString::from_vec(destination.name.clone()))?;
        let staged_name = EntryName::new(std::ffi::OsString::from_vec(stage.name.clone()))?;
        let custody = self.recovery_custody(parent, stage)?;
        let actual_destination = self.mutation_locator(parent, &name, Some(source))?;
        if actual_destination != *destination {
            return Err(FsError::Denied);
        }
        self.captured_restore_effect(&custody, parent, || {
            if let Some(expected) = owner_key
                && *lock(&custody.owner_key)? != Some(expected)
            {
                return Err(FsError::Denied);
            }
            let victim = NativeIdentity::of(
                &custody
                    .directory
                    .symlink_metadata(staged_name.as_os_str())
                    .map_err(io_error)?,
            );
            let published = NativeIdentity::of(
                &Self::directory(&parent.lease)?
                    .symlink_metadata(name.as_os_str())
                    .map_err(io_error)?,
            );
            if Some(victim) != stage.identity || published != source {
                return Err(FsError::StaleEntry);
            }
            effect((
                [source.device, source.inode],
                [victim.device, victim.inode],
                [published.device, published.inode],
            ))
        })
    }
    pub(crate) fn removal_directory(
        &self,
        entry: &GrantedEntry,
    ) -> Result<Arc<RemovalCustody>, FsError> {
        let table = self.call()?;
        let custody = lock(&self.inner.removal)?
            .clone()
            .ok_or(FsError::Unsupported(
                "protected removal custody was not admitted",
            ))?;
        self.validate(&table, &custody.root, Some(FsRight::Purge))?;
        self.validate(&table, entry.lease(), Some(FsRight::Purge))?;
        if !entry.lease().descends_from(&custody.root) {
            return Err(FsError::Denied);
        }
        custody.check()?;
        Ok(custody)
    }
    pub(crate) fn created_temporary_custody(
        &self,
        parent: &DirectoryGrant,
        created: &GrantedEntry,
    ) -> Result<Arc<RemovalCustody>, FsError> {
        let table = self.call()?;
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        self.validate(&table, &custody.root, Some(FsRight::Enumerate))?;
        self.validate(&table, &parent.lease, Some(FsRight::Create))?;
        self.validate(&table, created.lease(), Some(FsRight::Rename))?;
        if !parent.lease.descends_from(&custody.root)
            || !created.lease().descends_from(&parent.lease)
        {
            return Err(FsError::Denied);
        }
        custody.check()?;
        Ok(custody)
    }
    pub(crate) fn created_temporary_effect<T>(
        &self,
        custody: &RemovalCustody,
        parent: &DirectoryGrant,
        created: &GrantedEntry,
        effect: impl FnOnce() -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        let table = self.call()?;
        self.validate(&table, &custody.root, Some(FsRight::Enumerate))?;
        self.validate(&table, &parent.lease, Some(FsRight::Create))?;
        self.validate(&table, created.lease(), Some(FsRight::Rename))?;
        if !parent.lease.descends_from(&custody.root)
            || !created.lease().descends_from(&parent.lease)
        {
            return Err(FsError::Denied);
        }
        custody.check()?;
        effect()
    }
    pub(crate) fn replacement_directory(
        &self,
        temporary: &GrantedEntry,
    ) -> Result<Arc<RemovalCustody>, FsError> {
        let table = self.call()?;
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        self.validate(&table, &custody.root, Some(FsRight::Replace))?;
        self.validate(&table, temporary.lease(), Some(FsRight::Rename))?;
        self.validate(&table, temporary.lease(), Some(FsRight::Write))?;
        if !temporary.lease().descends_from(&custody.root) {
            return Err(FsError::Denied);
        }
        custody.check()?;
        Ok(custody)
    }
    pub(crate) fn retained_journal_roots(
        &self,
        root: &DirectoryGrant,
        journal: &std::fs::File,
    ) -> Result<(NativeIdentity, NativeIdentity), FsError> {
        let table = self.call()?;
        self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        if !Arc::ptr_eq(&root.lease.record, &custody.root.record) {
            return Err(FsError::Denied);
        }
        custody.check()?;
        let actual = journal.metadata().map_err(io_error)?;
        if !actual.is_dir()
            || actual.uid() != rustix::process::geteuid().as_raw()
            || actual.mode() & 0o7777 != 0o700
        {
            return Err(FsError::Denied);
        }
        let copy = Dir::from_std_file(journal.try_clone().map_err(io_error)?);
        let guard = parent(&copy)?;
        if host_identity(&guard.metadata().map_err(io_error)?) != custody.guard_identity {
            return Err(FsError::Denied);
        }
        let home = root.lease.object()?.identity();
        if actual.dev() != home.device {
            return Err(FsError::Denied);
        }
        #[cfg(target_os = "linux")]
        if mount(journal.as_raw_fd())? != mount(Self::directory(&root.lease)?.as_raw_fd())? {
            return Err(FsError::Denied);
        }
        Ok((home, custody.identity))
    }
    pub(crate) fn recovery_scope(&self, parent: &DirectoryGrant) -> Result<NativeLocator, FsError> {
        let table = self.call()?;
        self.validate(&table, &parent.lease, Some(FsRight::Enumerate))?;
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        self.validate(&table, &custody.root, Some(FsRight::Enumerate))?;
        if !parent.lease.descends_from(&custody.root) {
            return Err(FsError::Denied);
        }
        custody.check()?;
        let label = EntryName::new(std::ffi::OsString::from("recovery-scope"))?;
        self.locator_locked(&parent.lease, &label, None)
    }
    pub(crate) fn recovery_custody(
        &self,
        parent: &DirectoryGrant,
        stage: &NativeLocator,
    ) -> Result<Arc<RemovalCustody>, FsError> {
        let table = self.call()?;
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        self.validate(&table, &custody.root, Some(FsRight::Enumerate))?;
        self.validate(&table, &parent.lease, Some(FsRight::Create))?;
        if !parent.lease.descends_from(&custody.root)
            || stage.root != custody.identity
            || !stage.parents.is_empty()
        {
            return Err(FsError::Denied);
        }
        custody.check()?;
        Ok(custody)
    }
    pub(crate) fn captured_restore_effect<T>(
        &self,
        custody: &RemovalCustody,
        parent: &DirectoryGrant,
        action: impl FnOnce() -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        let table = self.call()?;
        self.validate(&table, &custody.root, Some(FsRight::Enumerate))?;
        self.validate(&table, &parent.lease, Some(FsRight::Create))?;
        if !parent.lease.descends_from(&custody.root) {
            return Err(FsError::Denied);
        }
        custody.check()?;
        action()
    }
    pub(crate) fn captured_effect<T>(
        &self,
        custody: &RemovalCustody,
        action: impl FnOnce() -> Result<T, FsError>,
    ) -> Result<T, FsError> {
        let table = self.call()?;
        self.validate(&table, &custody.root, Some(FsRight::Purge))?;
        custody.check()?;
        action()
    }
}
