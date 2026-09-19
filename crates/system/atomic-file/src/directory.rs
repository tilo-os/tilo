use crate::identity::FileIdentity;
use crate::native;
use crate::publication::{Privacy, PublicationError, PublishMode};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::Path;

/// An admitted directory held by native handle for relative storage operations.
///
/// This preserves the admitted parent when its ambient path is renamed or
/// replaced. Publication still requires a framework-owned private namespace or
/// equivalent caller-held exclusion: a held parent alone cannot prevent a
/// hostile peer from mutating child names inside that directory.
pub struct StorageDirectory {
    pub(crate) inner: native::Directory,
}

impl StorageDirectory {
    /// Consume an already-authorized directory handle.
    pub fn from_file(file: File) -> io::Result<Self> {
        native::Directory::from_file(file).map(|inner| Self { inner })
    }

    /// Admit a trusted-host directory path and retain its opened handle.
    pub fn open(path: &Path) -> io::Result<Self> {
        native::Directory::open(path).map(|inner| Self { inner })
    }

    /// Validate current privacy through the retained directory handle, without
    /// reopening a path or changing permissions. `Inherit` adds no restriction.
    ///
    /// `OwnerOnly` requires the effective native principal and a supported native
    /// permission model. Linux admits ext2/3/4, Btrfs, XFS, tmpfs and OverlayFS;
    /// other profiles return `Unsupported`. This is permission admission, not
    /// durability certification. The host/backing namespace must be trusted.
    /// The cooperative owner must maintain permissions and child-name exclusion
    /// throughout use; this does not defeat a hostile same-principal process,
    /// privileged access, later ACL changes, or otherwise-authorized aliases.
    pub fn validate_privacy(&self, privacy: Privacy) -> io::Result<()> {
        self.inner.validate_privacy(privacy)
    }

    /// Validate effective-owner control of this held directory's namespace.
    /// Unlike `OwnerOnly`, group/other read and search are permitted. This
    /// changes no permissions and grants no lasting lease against mutation.
    /// Uses the same supported Unix profiles and conservative macOS ACL policy
    /// as private creation; private child admission remains independent.
    #[cfg(unix)]
    pub fn validate_namespace_control(&self) -> io::Result<()> {
        self.inner.validate_namespace_control()
    }

    /// Query the identity of the retained directory handle.
    pub fn identity(&self) -> io::Result<FileIdentity> {
        self.inner.identity().map(|inner| FileIdentity { inner })
    }

    /// Open one child component without following a symbolic link and validate
    /// the requested privacy semantics.
    pub fn open_read(&self, name: &OsStr, privacy: Privacy) -> io::Result<File> {
        self.inner.open_read(name, privacy)
    }

    /// Acquire a lease on a single regular, no-follow sentinel without truncation.
    /// This supplies coordination only, not a namespace durability acknowledgement.
    #[cfg(unix)]
    pub fn try_acquire_lease(
        &self,
        name: &OsStr,
        mode: crate::lease::LeaseMode,
    ) -> Result<crate::lease::FileLease, crate::lease::AcquireError> {
        let file = self
            .inner
            .open_writable(name, Privacy::Inherit, true, false)
            .map_err(crate::lease::AcquireError::Io)?;
        crate::lease::FileLease::try_acquire(file, mode)
    }

    /// Open an existing regular file for append through this held parent.
    #[cfg(unix)]
    pub fn open_append_existing(&self, name: &OsStr, privacy: Privacy) -> io::Result<File> {
        self.inner.open_writable(name, privacy, false, true)
    }

    /// Commit the exact existing file and its existing name, without publication.
    #[cfg(unix)]
    pub fn commit_existing(
        &self,
        name: &OsStr,
        expected: &FileIdentity,
        privacy: Privacy,
    ) -> Result<(), crate::ExistingCommitError> {
        crate::existing::commit(self, name, expected, privacy)
    }

    /// Durably publish bytes under one child component.
    pub fn publish(
        &self,
        name: &OsStr,
        bytes: &[u8],
        mode: PublishMode,
        privacy: Privacy,
    ) -> Result<(), PublicationError> {
        self.publish_with_mode(name, bytes, mode, privacy, None)
    }

    pub(crate) fn publish_with_mode(
        &self,
        name: &OsStr,
        bytes: &[u8],
        mode: PublishMode,
        privacy: Privacy,
        exact_mode: Option<u32>,
    ) -> Result<(), PublicationError> {
        native::publish(&self.inner, name, bytes, mode, privacy, exact_mode)
    }
}
