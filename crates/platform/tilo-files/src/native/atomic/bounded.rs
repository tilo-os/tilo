//! Bounded current privacy profiles for supported macOS and Linux filesystems.
//! This is not root/lease authority or proof of original host resource funding.
//!
//! The supported 64-bit macOS profile requires affirmative extended-security
//! volume capability and a complete known ACL layout, with no ALLOW or deferred
//! inheritance. Ordinary `Privacy::OwnerOnly` remains unchanged. Every operation
//! checks current held objects. The caller must retain original root/child-name
//! and writer-lease custody; this does not defeat a hostile same-principal actor.
//!
//! Linux supports the ext2/3/4 family, btrfs, XFS and tmpfs POSIX ACL profiles.
//! `OverlayFS` is excluded. tmpfs privacy is not persistent/reboot evidence.
//!
//! Syscall buffers are fixed stack arrays. Rust heap costs below describe
//! requested backing bytes, not kernel use or allocator caches. The installed
//! syscall stubs and SDK layout require platform requalification after changes.
//! Formatting/wrapping errors (including `into_io_error`) is outside these costs.
use crate::native::atomic::{FileIdentity, StorageDirectory, native};
use std::{ffi::OsStr, fs::File, io, os::unix::ffi::OsStrExt};

/// Move-only current privacy profile; never a caller or persistence receipt.
pub struct BoundedOwnerDirectory {
    pub(crate) directory: StorageDirectory,
}
/// Refusal retains the original directory handle and its native error.
pub struct AdmissionError {
    source: io::Error,
    directory: StorageDirectory,
}
impl AdmissionError {
    pub fn io_error(&self) -> &io::Error {
        &self.source
    }
    pub fn into_directory(self) -> StorageDirectory {
        self.directory
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
    /// Inspect the exact retained FD. This does not acquire a lease or sync data.
    ///
    /// # Errors
    ///
    /// Returns an [`AdmissionError`] retaining `directory` when its current
    /// privacy profile is not a supported bounded owner-only profile.
    pub fn admit(directory: StorageDirectory) -> Result<Self, AdmissionError> {
        match directory.inner.validate_bounded() {
            Ok(()) => Ok(Self { directory }),
            Err(source) => Err(AdmissionError { source, directory }),
        }
    }
    /// Query the retained directory's identity.
    ///
    /// # Errors
    ///
    /// Returns the native error when the held directory cannot be inspected.
    pub fn identity(&self) -> io::Result<FileIdentity> {
        self.directory.identity()
    }
    /// Revalidate the retained directory's bounded privacy profile.
    ///
    /// # Errors
    ///
    /// Returns the native error when the held directory no longer satisfies the
    /// bounded owner-only profile.
    pub fn validate(&self) -> io::Result<()> {
        self.directory.inner.validate_bounded()
    }
    /// Open a current existing private regular file for reading.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid name, a directory that fails validation,
    /// or a child that cannot be opened under the bounded file policy.
    pub fn open_read(&self, name: &OsStr) -> io::Result<File> {
        name_check(name)?;
        self.validate()?;
        self.directory
            .inner
            .open_read_policy(name, native::FilePolicy::BoundedOwner)
    }
    /// Open a current existing private regular sentinel, without create/truncate.
    /// The caller separately acquires its actual `FileLease`.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid name, a directory that fails validation,
    /// or a missing or non-private sentinel.
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
    /// Durably publish `bytes` as the private child `name`.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::native::atomic::PublicationError`] carrying the known publication state
    /// when validation, staging, rename or a durability barrier fails.
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
    /// Commit the exact existing private child and its name, without publication.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::native::atomic::ExistingCommitError`] retaining commit authority when
    /// the existing binding changed or a durability barrier failed.
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

// Keep original and generated stage names inside rustix's256-byte stack path.
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
    /// Validate this retained parent and open its current named child.
    /// This is privacy inspection, not a stable-name or writer-lease receipt.
    /// The operation requests no heap allocation.
    ///
    /// # Errors
    ///
    /// Returns [`ChildOpenError::Open`] when this parent fails validation or the
    /// child cannot be opened, and [`ChildOpenError::Admission`] when the child
    /// is not a bounded owner-only directory.
    pub fn open_directory_child(&self, name: &OsStr) -> Result<Self, ChildOpenError> {
        self.validate().map_err(ChildOpenError::Open)?;
        Self::open_child(&self.directory, name)
    }

    /// Open one existing child under the retained parent. The parent is borrowed,
    /// not qualified by this operation; the caller must keep its root custody.
    ///
    /// # Errors
    ///
    /// Returns [`ChildOpenError::Open`] for an invalid name or an unopenable
    /// child, and [`ChildOpenError::Admission`] when the child is not a bounded
    /// owner-only directory.
    pub fn open_child(parent: &StorageDirectory, name: &OsStr) -> Result<Self, ChildOpenError> {
        name_check(name).map_err(ChildOpenError::Open)?;
        let inner = parent
            .inner
            .open_directory(name)
            .map_err(ChildOpenError::Open)?;
        Self::admit(StorageDirectory { inner }).map_err(ChildOpenError::Admission)
    }
    /// Maximum requested heap bytes for the generated stage name. OS errors are
    /// inline; normal publication retains no name after rename/commit.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when `name` is not a valid bounded child name.
    pub fn publication_allocation_bytes(name: &OsStr) -> io::Result<usize> {
        name_check(name)?;
        Ok(name.as_bytes().len().saturating_add(37))
    }
    /// Exact requested heap bytes for the retained existing-commit name.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when `name` is not a valid bounded child name.
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
