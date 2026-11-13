//! Bounded current privacy profiles for supported macOS and Linux filesystems: not root
//! or lease authority, and no proof of the original host resource funding. The caller
//! keeps original root, child-name and writer-lease custody, and this does not defeat a
//! hostile same-principal actor. Linux supports the ext2/3/4 family, btrfs, XFS and
//! tmpfs POSIX ACL profiles, with `OverlayFS` excluded; tmpfs privacy is not reboot
//! evidence. Syscall buffers are fixed stack arrays.
use crate::native::atomic::{FileIdentity, StorageDirectory, native};
use std::{ffi::OsStr, fs::File, io, os::unix::ffi::OsStrExt};

/// A move-only current privacy profile, never a caller or persistence receipt.
pub struct BoundedOwnerDirectory {
    pub(crate) directory: StorageDirectory,
}
/// Why a directory was refused: its native error.
pub struct AdmissionError {
    source: io::Error,
}
impl AdmissionError {
    pub fn io_error(&self) -> &io::Error {
        &self.source
    }
}
impl std::fmt::Debug for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdmissionError")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}
impl BoundedOwnerDirectory {
    /// Inspects the exact retained descriptor; this acquires no lease and syncs no data.
    pub fn admit(directory: StorageDirectory) -> Result<Self, AdmissionError> {
        match directory.inner.validate_bounded() {
            Ok(()) => Ok(Self { directory }),
            Err(source) => Err(AdmissionError { source }),
        }
    }
    pub fn identity(&self) -> io::Result<FileIdentity> {
        self.directory.identity()
    }
    pub fn validate(&self) -> io::Result<()> {
        self.directory.inner.validate_bounded()
    }
    pub fn open_read(&self, name: &OsStr) -> io::Result<File> {
        name_check(name)?;
        self.validate()?;
        self.directory
            .inner
            .open_read_policy(name, native::FilePolicy::BoundedOwner)
    }
    /// Without create or truncate; the caller acquires the lease separately.
    pub fn open_lease_existing(&self, name: &OsStr) -> io::Result<File> {
        name_check(name)?;
        self.validate()?;
        self.directory.inner.open_writable_policy(
            name,
            native::FilePolicy::BoundedOwner,
            false,
            false,
        )
    }
}
#[cfg(test)]
#[cfg(target_os = "linux")]
mod linux_tests;
#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests;

impl BoundedOwnerDirectory {
    pub fn publish(
        &self,
        name: &OsStr,
        bytes: &[u8],
        mode: crate::native::atomic::PublishMode,
    ) -> Result<(), crate::native::atomic::PublicationError> {
        name_check(name)
            .and_then(|()| self.validate())
            .map_err(|e| {
                crate::native::atomic::PublicationError::new(
                    crate::native::atomic::PublicationState::NotPublished,
                    e,
                )
            })?;
        native::publish_policy(
            &self.directory.inner,
            name,
            bytes,
            mode,
            native::FilePolicy::BoundedOwner,
            None,
        )
    }
    pub fn commit_existing(
        &self,
        name: &OsStr,
        expected: &FileIdentity,
    ) -> Result<(), crate::native::atomic::ExistingCommitError> {
        crate::native::atomic::existing::commit_policy(
            &self.directory,
            name,
            expected,
            native::FilePolicy::BoundedOwner,
        )
    }
}

// Keeps original and generated stage names inside rustix's 256-byte stack path.
pub(crate) fn name_check(name: &OsStr) -> io::Result<()> {
    let b = name.as_bytes();
    if b.is_empty()
        || b.len() > 200
        || b == b"."
        || b == b".."
        || b.contains(&b'/')
        || b.contains(&0)
    {
        Err(io::ErrorKind::InvalidInput.into())
    } else {
        Ok(())
    }
}
impl BoundedOwnerDirectory {
    /// Validates this retained parent and opens its current named child: privacy inspection, not a
    /// stable-name handle.
    pub fn open_directory_child(&self, name: &OsStr) -> Result<Self, ChildOpenError> {
        self.validate().map_err(ChildOpenError::Open)?;
        Self::open_child(&self.directory, name)
    }

    /// The parent is borrowed, not qualified by this operation; the caller keeps its root custody.
    pub fn open_child(parent: &StorageDirectory, name: &OsStr) -> Result<Self, ChildOpenError> {
        name_check(name).map_err(ChildOpenError::Open)?;
        let inner = parent
            .inner
            .open_directory(name)
            .map_err(ChildOpenError::Open)?;
        Self::admit(StorageDirectory { inner }).map_err(ChildOpenError::Admission)
    }
    pub fn publication_allocation_bytes(name: &OsStr) -> io::Result<usize> {
        name_check(name)?;
        Ok(name.as_bytes().len().saturating_add(37))
    }
    /// The exact requested heap bytes for the retained existing-commit name.
    pub fn existing_commit_allocation_bytes(name: &OsStr) -> io::Result<usize> {
        name_check(name)?;
        Ok(name.as_bytes().len())
    }
}
#[derive(Debug)]
pub enum ChildOpenError {
    Open(io::Error),
    Admission(AdmissionError),
}
mod fresh;
#[cfg(target_os = "macos")]
pub use fresh::OwnerOnlyProvisioning;
pub use fresh::{FreshChildError, FreshChildState, FreshPrivateChild};

impl ChildOpenError {
    pub fn io_error(&self) -> &io::Error {
        match self {
            Self::Open(e) => e,
            Self::Admission(e) => e.io_error(),
        }
    }
}
impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}
impl std::error::Error for AdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}
impl std::fmt::Display for ChildOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.io_error().fmt(f)
    }
}
impl std::error::Error for ChildOpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.io_error())
    }
}
