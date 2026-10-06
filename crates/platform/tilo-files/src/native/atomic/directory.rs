use crate::native::atomic::identity::FileIdentity;
use crate::native::atomic::native;
use crate::native::atomic::publication::{Privacy, PublicationError, PublishMode};
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
    ///
    /// # Errors
    ///
    /// Returns an error when `file` is not a directory handle.
    pub fn from_file(file: File) -> io::Result<Self> {
        native::Directory::from_file(file).map(|inner| Self { inner })
    }

    /// Admit a trusted-host directory path and retain its opened handle.
    ///
    /// # Errors
    ///
    /// Returns the native error when `path` cannot be opened as a directory.
    pub fn open(path: &Path) -> io::Result<Self> {
        native::Directory::open(path).map(|inner| Self { inner })
    }

    /// Validate current privacy through the retained directory handle, without
    /// reopening a path or changing permissions. `Inherit` adds no restriction.
    ///
    /// `OwnerOnly` requires the effective native principal and a supported native
    /// permission model. Linux admits ext2/3/4, Btrfs, XFS, tmpfs and `OverlayFS`;
    /// other profiles return `Unsupported`. This is permission admission, not
    /// durability certification. The host/backing namespace must be trusted.
    /// The cooperative owner must maintain permissions and child-name exclusion
    /// throughout use; this does not defeat a hostile same-principal process,
    /// privileged access, later ACL changes, or otherwise-authorized aliases.
    ///
    /// # Errors
    ///
    /// Returns `Unsupported` for an unsupported permission profile and
    /// `PermissionDenied` when the directory is not private to the owner.
    pub fn validate_privacy(&self, privacy: Privacy) -> io::Result<()> {
        self.inner.validate_privacy(privacy)
    }

    /// Validate effective-owner control of this held directory's namespace.
    /// Unlike `OwnerOnly`, group/other read and search are permitted. This
    /// changes no permissions and grants no lasting lease against mutation.
    /// Uses the same supported Unix profiles and conservative macOS ACL policy
    /// as private creation; private child admission remains independent.
    ///
    /// # Errors
    ///
    /// Returns an error when the effective owner does not control this
    /// directory's namespace or the profile is unsupported.
    #[cfg(unix)]
    pub fn validate_namespace_control(&self) -> io::Result<()> {
        self.inner.validate_namespace_control()
    }

    /// Query the identity of the retained directory handle.
    ///
    /// # Errors
    ///
    /// Returns the native error when the handle cannot be inspected.
    pub fn identity(&self) -> io::Result<FileIdentity> {
        self.inner.identity().map(|inner| FileIdentity { inner })
    }

    /// Open one child component without following a symbolic link and validate
    /// the requested privacy semantics.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or symbolic-link child, or one that fails
    /// the requested privacy.
    pub fn open_read(&self, name: &OsStr, privacy: Privacy) -> io::Result<File> {
        self.inner.open_read(name, privacy)
    }

    /// Acquire a lease on a single regular, no-follow sentinel without truncation.
    /// This supplies coordination only, not a namespace durability acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns [`crate::native::atomic::lease::AcquireError::Busy`] for a conflicting lease and
    /// [`crate::native::atomic::lease::AcquireError::Io`] when the sentinel cannot be opened or
    /// locked.
    #[cfg(unix)]
    pub fn try_acquire_lease(
        &self,
        name: &OsStr,
        mode: crate::native::atomic::lease::LeaseMode,
    ) -> Result<crate::native::atomic::lease::FileLease, crate::native::atomic::lease::AcquireError>
    {
        let file = self
            .inner
            .open_writable(name, Privacy::Inherit, true, false)
            .map_err(crate::native::atomic::lease::AcquireError::Io)?;
        crate::native::atomic::lease::FileLease::try_acquire(file, mode)
    }

    /// Open an existing regular file for append through this held parent.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing, non-regular or insufficiently private
    /// child.
    #[cfg(unix)]
    pub fn open_append_existing(&self, name: &OsStr, privacy: Privacy) -> io::Result<File> {
        self.inner.open_writable(name, privacy, false, true)
    }

    /// Commit the exact existing file and its existing name, without publication.
    ///
    /// # Errors
    ///
    /// Returns a [`crate::native::atomic::ExistingCommitError`] retaining commit authority when
    /// the binding changed or a durability barrier failed.
    #[cfg(unix)]
    pub fn commit_existing(
        &self,
        name: &OsStr,
        expected: &FileIdentity,
        privacy: Privacy,
    ) -> Result<(), crate::native::atomic::ExistingCommitError> {
        crate::native::atomic::existing::commit(self, name, expected, privacy)
    }

    /// Durably publish bytes under one child component.
    ///
    /// # Errors
    ///
    /// Returns a [`PublicationError`] carrying the known publication state when
    /// staging, rename or a durability barrier fails.
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
