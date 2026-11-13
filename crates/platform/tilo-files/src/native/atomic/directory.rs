use crate::native::atomic::identity::FileIdentity;
use crate::native::atomic::native;
use crate::native::atomic::publication::{Privacy, PublicationError, PublishMode};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::Path;

/// An admitted directory held by native handle for relative storage operations, which stays the
/// admitted parent when its ambient path is renamed or replaced. Publication still needs a
/// framework-owned private namespace or equivalent caller-held exclusion: a held parent alone cannot
/// stop a hostile peer from mutating child names.
pub struct StorageDirectory {
    pub(crate) inner: native::Directory,
}

impl StorageDirectory {
    pub fn from_file(file: File) -> io::Result<Self> {
        native::Directory::from_file(file).map(|inner| Self { inner })
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        native::Directory::open(path).map(|inner| Self { inner })
    }

    /// Through the retained handle, without reopening a path or changing permissions.
    pub fn validate_privacy(&self, privacy: Privacy) -> io::Result<()> {
        self.inner.validate_privacy(privacy)
    }

    /// Unlike `OwnerOnly`, group and other read and search are permitted. This changes no permissions
    /// and grants no lasting lease against mutation, and private child admission remains independent.
    #[cfg(unix)]
    pub fn validate_namespace_control(&self) -> io::Result<()> {
        self.inner.validate_namespace_control()
    }

    /// Another exact owner's held directory, read-only, root only. This grants no publication authority
    /// and changes no ownership or policy: `OwnerOnly` requires private DAC and ACL mode, `Inherit`
    /// requires owner control. The caller must obtain the owner and handle on an authenticated channel,
    /// else it gets `PermissionDenied` or `Unsupported`.
    #[cfg(target_os = "linux")]
    pub fn validate_observed_owner(&self, owner: u32, privacy: Privacy) -> io::Result<()> {
        self.inner.validate_observed_owner(owner, privacy)
    }

    pub fn identity(&self) -> io::Result<FileIdentity> {
        self.inner.identity().map(|inner| FileIdentity { inner })
    }

    /// Without following a symbolic link, validating the requested privacy semantics.
    pub fn open_read(&self, name: &OsStr, privacy: Privacy) -> io::Result<File> {
        self.inner.open_read(name, privacy)
    }

    /// Coordination only, not a namespace durability acknowledgement.
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

    #[cfg(unix)]
    pub fn open_append_existing(&self, name: &OsStr, privacy: Privacy) -> io::Result<File> {
        self.inner.open_writable(name, privacy, false, true)
    }

    /// Commits the exact existing file and its existing name, without publication.
    #[cfg(unix)]
    pub fn commit_existing(
        &self,
        name: &OsStr,
        expected: &FileIdentity,
        privacy: Privacy,
    ) -> Result<(), crate::native::atomic::ExistingCommitError> {
        crate::native::atomic::existing::commit(self, name, expected, privacy)
    }

    pub fn publish(
        &self,
        name: &OsStr,
        bytes: &[u8],
        mode: PublishMode,
        privacy: Privacy,
    ) -> Result<(), PublicationError> {
        self.publish_with_mode(name, bytes, mode, privacy, None)
    }

    /// Uses the same retained directory and publication path as [`Self::publish`].
    pub fn publish_with_permissions(
        &self,
        name: &OsStr,
        bytes: &[u8],
        mode: PublishMode,
        privacy: Privacy,
        permissions: u32,
    ) -> Result<(), PublicationError> {
        self.publish_with_mode(name, bytes, mode, privacy, Some(permissions))
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn publish_atomic_save(
        &self,
        name: &OsStr,
        bytes: &[u8],
        permissions: u32,
    ) -> Result<(), PublicationError> {
        native::publish_atomic_save(&self.inner, name, bytes, permissions)
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

#[cfg(all(test, target_os = "linux"))]
mod retained_foundation_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    #[test]
    fn explicit_permissions_publish_through_held_parent_after_name_replacement() {
        let root = tempfile::tempdir().unwrap();
        let named = root.path().join("store");
        std::fs::create_dir(&named).unwrap();
        let store = StorageDirectory::open(&named).unwrap();
        store.validate_namespace_control().unwrap();
        let retained = root.path().join("retained");
        std::fs::rename(&named, &retained).unwrap();
        std::fs::create_dir(&named).unwrap();
        store
            .publish_with_permissions(
                OsStr::new("record"),
                b"actual held publication",
                PublishMode::IfAbsent,
                Privacy::Inherit,
                0o640,
            )
            .unwrap();
        let record = retained.join("record");
        assert_eq!(std::fs::read(&record).unwrap(), b"actual held publication");
        assert_eq!(std::fs::metadata(&record).unwrap().mode() & 0o7777, 0o640);
        assert!(!named.join("record").exists());
        assert_eq!(std::fs::read_dir(&retained).unwrap().count(), 1);
    }

    #[test]
    fn public_permissions_refuse_private_publication_and_retire_stage() {
        let root = tempfile::tempdir().unwrap();
        let store = StorageDirectory::open(root.path()).unwrap();
        let result = store.publish_with_permissions(
            OsStr::new("record"),
            b"must remain unpublished",
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
            0o644,
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn observed_owner_requires_real_root_and_rechecks_held_permissions() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = StorageDirectory::open(root.path()).unwrap();
        let owner = std::fs::metadata(root.path()).unwrap().uid();
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            assert_eq!(
                store
                    .validate_observed_owner(owner, Privacy::OwnerOnly)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        } else {
            store
                .validate_observed_owner(owner, Privacy::OwnerOnly)
                .unwrap();
            assert_eq!(
                store
                    .validate_observed_owner(owner.checked_add(1).unwrap(), Privacy::OwnerOnly)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o770)).unwrap();
            assert_eq!(
                store
                    .validate_observed_owner(owner, Privacy::Inherit)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
        }
    }
}
