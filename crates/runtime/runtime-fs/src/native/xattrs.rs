//! Bounded original-user metadata access. No file-content methods or descriptors
//! leave this module. Refusal is safer than replacing with weaker permissions.
#[cfg(target_os = "linux")]
use super::{EntryName, FsRight, NativeIdentity, NativeObject, OFlags, io_error};
use super::{FsError, FsSession, GrantedEntry};
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;

#[cfg(target_os = "linux")]
const VALUE_LIMIT: usize = 65_536;
#[cfg(target_os = "linux")]
const ATTRIBUTE_LIMIT: usize = 128;
#[cfg(target_os = "linux")]
const TOTAL_LIMIT: usize = 1_048_576;
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct NativeAttributes(pub(super) BTreeMap<Vec<u8>, Vec<u8>>);
#[cfg(target_os = "linux")]
fn result(value: libc::ssize_t) -> Result<usize, FsError> {
    if value < 0 {
        Err(io_error(std::io::Error::last_os_error()))
    } else {
        usize::try_from(value).map_err(|_| FsError::LimitExceeded)
    }
}
#[cfg(target_os = "linux")]
fn observe(fd: &impl AsRawFd) -> Result<NativeAttributes, FsError> {
    let mut list = vec![0_u8; VALUE_LIMIT];
    // SAFETY: the borrowed descriptor cannot outlive its owner.
    let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd.as_raw_fd()) };
    let flags = rustix::fs::fcntl_getfl(borrowed).map_err(|e| io_error(e.into()))?;
    // Legacy f*xattr rejects O_PATH. Follow only this owned descriptor through
    // procfs instead of reopening the victim for content access. Kernel xattr
    // permissions still apply, including refusal of unreadable user attributes.
    let path = flags
        .contains(OFlags::PATH)
        .then(|| CString::new(format!("/proc/self/fd/{}", fd.as_raw_fd())))
        .transpose()
        .map_err(|_| FsError::StoreUnavailable)?;
    // SAFETY: held descriptor, owned procfs path and bounded writable buffer.
    let size = result(unsafe {
        if let Some(path) = &path {
            libc::listxattr(path.as_ptr(), list.as_mut_ptr().cast(), list.len())
        } else {
            libc::flistxattr(fd.as_raw_fd(), list.as_mut_ptr().cast(), list.len())
        }
    })?;
    if size > list.len() {
        return Err(FsError::LimitExceeded);
    }
    list.truncate(size);
    if !list.is_empty() && list.last() != Some(&0) {
        return Err(FsError::StoreUnavailable);
    }
    let mut attributes = BTreeMap::new();
    let mut total = size;
    for raw in list.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        if raw.len() > 255 || attributes.len() >= ATTRIBUTE_LIMIT || attributes.contains_key(raw) {
            return Err(FsError::LimitExceeded);
        }
        // These signatures authenticate original content or other metadata.
        // Reusing them on changed bytes cannot preserve their meaning.
        if raw == b"security.ima" || raw == b"security.evm" {
            return Err(FsError::Unsupported(
                "content-bound integrity metadata requires its system owner",
            ));
        }
        let name = CString::new(raw).map_err(|_| FsError::StoreUnavailable)?;
        let mut value = vec![0_u8; VALUE_LIMIT];
        // SAFETY: owned NUL-terminated metadata name, actual held descriptor and
        // bounded live buffer. This observes xattrs, never file contents.
        let length = result(unsafe {
            if let Some(path) = &path {
                libc::getxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            } else {
                libc::fgetxattr(
                    fd.as_raw_fd(),
                    name.as_ptr(),
                    value.as_mut_ptr().cast(),
                    value.len(),
                )
            }
        })?;
        if length > value.len() {
            return Err(FsError::LimitExceeded);
        }
        value.truncate(length);
        total = total.checked_add(length).ok_or(FsError::LimitExceeded)?;
        if total > TOTAL_LIMIT {
            return Err(FsError::LimitExceeded);
        }
        attributes.insert(raw.to_vec(), value);
    }
    Ok(NativeAttributes(attributes))
}
impl FsSession {
    #[cfg(target_os = "linux")]
    pub(crate) fn replacement_captured_attributes(
        &self,
        victim: &GrantedEntry,
        custody: &super::removal::RemovalCustody,
        stage: &EntryName,
        expected: NativeIdentity,
    ) -> Result<NativeAttributes, FsError> {
        let table = self.call()?;
        self.validate(&table, victim.lease(), Some(FsRight::Replace))?;
        self.metadata_locked(&table, victim.lease())?;
        if !victim.lease().descends_from(&custody.root) {
            return Err(FsError::Denied);
        }
        custody.check()?;
        let NativeObject::File {
            descriptor,
            identity,
        } = victim.lease().object()?
        else {
            return Err(FsError::Unsupported(
                "regular captured victim metadata required",
            ));
        };
        if identity != expected {
            return Err(FsError::StaleEntry);
        }
        let metadata = custody
            .directory
            .symlink_metadata(stage.as_os_str())
            .map_err(io_error)?;
        if NativeIdentity::of(&metadata) != identity {
            return Err(FsError::StaleEntry);
        }
        let flags = rustix::fs::fcntl_getfl(&*descriptor).map_err(|e| io_error(e.into()))?;
        if flags.contains(OFlags::PATH) {
            self.validate(&table, victim.lease(), Some(FsRight::Write))?;
        }
        let borrowed = &*descriptor;
        let stat = rustix::fs::fstat(borrowed).map_err(|e| io_error(e.into()))?;
        if [stat.st_dev, stat.st_ino] != [identity.device, identity.inode] {
            return Err(FsError::StaleEntry);
        }
        let attributes = observe(borrowed)?;
        self.validate(&table, victim.lease(), Some(FsRight::Replace))?;
        custody.check()?;
        let metadata = custody
            .directory
            .symlink_metadata(stage.as_os_str())
            .map_err(io_error)?;
        if NativeIdentity::of(&metadata) != identity {
            return Err(FsError::StaleEntry);
        }
        Ok(attributes)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn replacement_attributes(
        &self,
        victim: &GrantedEntry,
    ) -> Result<NativeAttributes, FsError> {
        let source = self.mutation_source(victim, FsRight::Replace)?;
        let table = self.call()?;
        self.validate(&table, victim.lease(), Some(FsRight::Replace))?;
        self.metadata_locked(&table, victim.lease())?;
        let NativeObject::File {
            descriptor,
            identity,
        } = source.object
        else {
            return Err(FsError::Unsupported(
                "regular replacement victim metadata required",
            ));
        };
        let flags = rustix::fs::fcntl_getfl(&*descriptor).map_err(|e| io_error(e.into()))?;
        if flags.contains(OFlags::PATH) {
            self.validate(&table, victim.lease(), Some(FsRight::Write))?;
        }
        let borrowed = &*descriptor;
        let before = rustix::fs::fstat(borrowed).map_err(|e| io_error(e.into()))?;
        if [before.st_dev, before.st_ino] != [identity.device, identity.inode] {
            return Err(FsError::StaleEntry);
        }
        let attributes = observe(borrowed)?;
        self.validate(&table, victim.lease(), Some(FsRight::Replace))?;
        let after = source
            .parent
            .symlink_metadata(source.name.as_os_str())
            .map_err(io_error)?;
        if NativeIdentity::of(&after) != identity {
            return Err(FsError::StaleEntry);
        }
        Ok(attributes)
    }
}
#[cfg(not(target_os = "linux"))]
impl FsSession {
    pub(crate) fn replacement_attributes(
        &self,
        _: &GrantedEntry,
    ) -> Result<NativeAttributes, FsError> {
        self.check_alive()?;
        Err(FsError::Unsupported(
            "replacement attribute preservation requires Linux",
        ))
    }
}
impl NativeAttributes {
    #[cfg(target_os = "linux")]
    pub(crate) fn apply(&self, fd: impl AsRawFd) -> Result<(), FsError> {
        let current = observe(&fd)?;
        for name in current.0.keys().filter(|n| !self.0.contains_key(*n)) {
            let name = CString::new(name.clone()).map_err(|_| FsError::StoreUnavailable)?;
            // SAFETY: actual held protected temporary and owned xattr name.
            if unsafe { libc::fremovexattr(fd.as_raw_fd(), name.as_ptr()) } < 0 {
                return Err(io_error(std::io::Error::last_os_error()));
            }
        }
        for (name, value) in &self.0 {
            if current.0.get(name) == Some(value) {
                continue;
            }
            let name = CString::new(name.clone()).map_err(|_| FsError::StoreUnavailable)?;
            // SAFETY: actual held protected temporary, owned name and bounded
            // value. The original UID's kernel rights alone permit this change.
            if unsafe {
                libc::fsetxattr(
                    fd.as_raw_fd(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            } < 0
            {
                return Err(io_error(std::io::Error::last_os_error()));
            }
        }
        if observe(&fd)? != *self {
            return Err(FsError::StaleEntry);
        }
        drop(fd);
        Ok(())
    }
}
