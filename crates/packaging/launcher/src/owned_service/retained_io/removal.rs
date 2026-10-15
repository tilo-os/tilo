//! Root-created private namespace on the actual admitted source filesystem.
//! Files enter only through original-credential atomic rename, never root reads.
use super::{RetainedIoAuthority, Token, refused, root};
use serde::{Deserialize, Serialize};
use std::{
    ffi::CString,
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    sync::Arc,
};
pub(super) const FD: i32 = 56;
pub(super) const JOURNAL_FD: i32 = 57;
pub(super) const SOURCE_DATA_FD: i32 = 58;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Facts {
    name: String,
    guard: (u64, u64),
    leaf: (u64, u64),
    journal: (u64, u64),
}
struct Custody {
    source: File,
    guard: advisory_lock::FileLock,
    leaf: File,
    journal: File,
    facts: Facts,
    authority: Arc<dyn RetainedIoAuthority>,
}
pub(super) struct Removal(Option<Custody>);
/// Opaque root-held namespace witness; neither names nor serialized claims can
/// construct it. It retains no flock, so an authorized later worker can recover.
pub struct RetainedNativeOwner {
    source: File,
    guard: File,
    leaf: File,
    journal: File,
    facts: Facts,
}
impl RetainedNativeOwner {
    /// Observe only namespace metadata after the worker has actually stopped.
    /// This does not read file contents or confer a restore/delete capability.
    ///
    /// # Errors
    /// Refuses non-root callers, changed or unprotected namespaces, and entries
    /// still requiring recovery; propagates namespace metadata I/O failures.
    pub fn confirm_retirement(&self) -> io::Result<()> {
        root()?;
        check_namespace(
            &self.source,
            &self.guard,
            &self.leaf,
            &self.journal,
            &self.facts,
        )?;
        if !empty(&self.leaf)? {
            return Err(refused(
                "Captured native entries require recovery; source custody retained",
            ));
        }
        Ok(())
    }
}
fn check_namespace(
    source: &File,
    guard: &File,
    leaf: &File,
    journal: &File,
    facts: &Facts,
) -> io::Result<()> {
    let name = CString::new(facts.name.as_bytes()).map_err(io::Error::other)?;
    if id(&open(source, &name)?)? != facts.guard
        || id(&open(guard, c"entries")?)? != facts.leaf
        || id(&open(guard, c"journal")?)? != facts.journal
        || id(leaf)? != facts.leaf
        || id(journal)? != facts.journal
    {
        return Err(refused("Held removal namespace changed"));
    }
    let metadata = guard.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o7777 != 0o700 {
        return Err(refused("Removal namespace is not root private"));
    }
    Ok(())
}
fn id(file: &File) -> io::Result<(u64, u64)> {
    let m = file.metadata()?;
    Ok((m.dev(), m.ino()))
}
fn open(parent: &File, name: &std::ffi::CStr) -> io::Result<File> {
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(raw) })
}
fn empty(file: &File) -> io::Result<bool> {
    use std::os::fd::IntoRawFd;

    let fresh = open(file, c".")?;
    let raw = fresh.into_raw_fd();
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        unsafe { libc::close(raw) };
        return Err(io::Error::last_os_error());
    }
    let mut result = true;
    loop {
        unsafe {
            *libc::__errno_location() = 0;
        }
        let row = unsafe { libc::readdir(stream) };
        if row.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(0) {
                unsafe { libc::closedir(stream) };
                return Err(error);
            }
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*row).d_name.as_ptr()) };
        if name != c"." && name != c".." {
            result = false;
            break;
        }
    }
    if unsafe { libc::closedir(stream) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(result)
}
fn mount_id(file: &File) -> io::Result<u64> {
    let mut value = std::mem::MaybeUninit::<libc::statx>::uninit();
    if unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            value.as_mut_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let value = unsafe { value.assume_init() };
    if value.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(refused("Original source mount identity is unavailable"));
    }
    Ok(value.stx_mnt_id)
}
impl Removal {
    pub(super) fn create(
        home: &File,
        token: &Token,
        authority: Arc<dyn RetainedIoAuthority>,
    ) -> io::Result<Self> {
        root()?;
        authority.check()?;
        let source = authority.removal_namespace()?;
        if mount_id(home)? != mount_id(&source)? {
            return Err(refused(
                "Protected native owner differs from original source mount",
            ));
        }
        let storage = tilo_files::native::atomic::StorageDirectory::from_file(source.try_clone()?)?;
        storage.validate_namespace_control()?;
        storage.validate_privacy(tilo_files::native::atomic::Privacy::OwnerOnly)?;
        let suffix = hex::encode(
            token
                .native_owner_sha256
                .ok_or_else(|| refused("Stable native owner is absent"))?,
        );
        let name = format!("retained-native-{suffix}");
        let raw = CString::new(name.as_bytes()).map_err(io::Error::other)?;
        if unsafe { libc::mkdirat(source.as_raw_fd(), raw.as_ptr(), 0o700) } != 0
            && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
        {
            return Err(io::Error::last_os_error());
        }
        let guard = advisory_lock::FileLock::try_exclusive(open(&source, &raw)?)
            .map_err(io::Error::from)?;
        let m = guard.metadata()?;
        if m.uid() != 0 || m.mode() & 0o7777 != 0o700 {
            return Err(refused(
                "Source filesystem cannot protect removal namespace",
            ));
        }
        let leaf = delegated_leaf(&guard, c"entries", token)?;
        let journal = delegated_leaf(&guard, c"journal", token)?;
        leaf.sync_all()?;
        journal.sync_all()?;
        guard.sync_all()?;
        source.sync_all()?;
        let facts = Facts {
            name,
            guard: id(&guard)?,
            leaf: id(&leaf)?,
            journal: id(&journal)?,
        };
        let this = Self(Some(Custody {
            source: source.try_clone()?,
            guard,
            leaf,
            journal,
            facts,
            authority,
        }));
        this.check()?;
        let custody = this.0.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        custody.authority.retain_native_owner(RetainedNativeOwner {
            source: open(&custody.source, c".")?,
            guard: open(&custody.guard, c".")?,
            leaf: open(&custody.leaf, c".")?,
            journal: open(&custody.journal, c".")?,
            facts: custody.facts.clone(),
        })?;
        Ok(this)
    }
    pub(super) fn descriptor(&self) -> io::Result<i32> {
        Ok(self
            .0
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .leaf
            .as_raw_fd())
    }
    pub(super) fn journal_descriptor(&self) -> io::Result<i32> {
        Ok(self
            .0
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .journal
            .as_raw_fd())
    }
    pub(super) fn facts(&self) -> io::Result<Facts> {
        Ok(self
            .0
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .facts
            .clone())
    }
    pub(super) fn check(&self) -> io::Result<()> {
        root()?;
        let c = self.0.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        c.authority.check()?;
        check_namespace(&c.source, &c.guard, &c.leaf, &c.journal, &c.facts)?;
        Ok(())
    }
    pub(super) fn confirm_empty(&self) -> io::Result<()> {
        root()?;
        let custody = self.0.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        // Target revocation must stop I/O, but cannot prevent observing retained
        // source namespace obligations after confirmed worker death.
        check_namespace(
            &custody.source,
            &custody.guard,
            &custody.leaf,
            &custody.journal,
            &custody.facts,
        )?;
        if !empty(&custody.leaf)? {
            return Err(refused(
                "Captured removal requires native recovery; source custody retained",
            ));
        }
        Ok(())
    }
}
// Both leaves persist under the actual protected source-record namespace.
// Dropping a worker releases only the exclusive session lock. It never discards
// operation receipts or captured entries; process-held lists are not recovery.
// Root registration retirement and canonical history adoption remain required
// before this draft can enable destructive operations.
fn delegated_leaf(guard: &File, name: &std::ffi::CStr, token: &Token) -> io::Result<File> {
    let created = unsafe { libc::mkdirat(guard.as_raw_fd(), name.as_ptr(), 0o700) } == 0;
    if !created && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
        return Err(io::Error::last_os_error());
    }
    let file = open(guard, name)?;
    if created
        && (unsafe { libc::fchown(file.as_raw_fd(), token.uid, token.gid) } != 0
            || unsafe { libc::fchmod(file.as_raw_fd(), 0o700) } != 0)
    {
        return Err(io::Error::last_os_error());
    }
    let metadata = file.metadata()?;
    if metadata.uid() != token.uid
        || metadata.gid() != token.gid
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(refused(
            "Persistent native owner differs from original credentials",
        ));
    }
    Ok(file)
}
// The signed shim receives descriptors from the same original source mount;
// independently binding each pathname would produce cross-mount EXDEV.
pub(super) fn prepare(facts: &Facts) -> io::Result<()> {
    root()?;
    let raw = unsafe { libc::fcntl(FD, libc::F_DUPFD_CLOEXEC, 128) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let leaf = unsafe { File::from_raw_fd(raw) };
    if id(&leaf)? != facts.leaf {
        return Err(refused("Sealed removal leaf differs"));
    }
    let raw = unsafe {
        libc::openat(
            leaf.as_raw_fd(),
            c"..".as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let guard = unsafe { File::from_raw_fd(raw) };
    let m = guard.metadata()?;
    if id(&guard)? != facts.guard || m.uid() != 0 || m.mode() & 0o7777 != 0o700 {
        return Err(refused("Sealed removal guard differs"));
    }
    let raw = unsafe { libc::fcntl(JOURNAL_FD, libc::F_DUPFD_CLOEXEC, 128) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let journal = unsafe { File::from_raw_fd(raw) };
    if id(&journal)? != facts.journal {
        return Err(refused("Sealed native journal differs"));
    }
    let parent = open(&journal, c"..")?;
    if id(&parent)? != facts.guard {
        return Err(refused("Native journal guard differs"));
    }
    Ok(())
}
