//! Validate an inherited image-update lock without releasing its parent transaction.
//!
//! This proves the exact private transaction inode and shared open description.
//! Callers independently enforce root authority and the canonical update-lock path.
use std::{
    fs::File,
    os::fd::{FromRawFd, RawFd},
    os::unix::fs::MetadataExt,
    path::Path,
};

#[cfg(target_os = "linux")]
fn open_readonly_without_symlinks(path: &Path) -> std::io::Result<File> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _};
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "lock path contains an embedded NUL byte",
        )
    })?;
    crate::nofollow::open(
        None,
        &path,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
        0,
        false,
    )
}

#[cfg(not(target_os = "linux"))]
fn open_readonly_without_symlinks(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "inherited update lock requires Linux whole-path symlink refusal",
    ))
}

/// Duplicate an already-held exclusive transaction lock, preserving parent custody.
/// Closing the returned descriptor never explicitly unlocks its open description.
///
/// # Errors
/// Refuses invalid descriptors, unlocked or foreign descriptions, nonprivate files,
/// replaced paths, and symlinks anywhere in the transaction path.
pub fn inherit(fd: RawFd, path: &Path) -> Result<std::fs::File, String> {
    // SAFETY: fcntl duplicates an existing descriptor without transferring the
    // caller's ownership; successful results are new owned descriptors.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err("cannot duplicate inherited update lock descriptor".into());
    }
    // SAFETY: the successful fcntl result is uniquely owned here.
    let inherited = unsafe { std::fs::File::from_raw_fd(duplicate) };
    let observed = inherited.metadata().map_err(|error| error.to_string())?;
    let probe = open_readonly_without_symlinks(path).map_err(|error| error.to_string())?;
    let expected = probe.metadata().map_err(|error| error.to_string())?;
    if !observed.is_file()
        || observed.uid() != rustix::process::geteuid().as_raw()
        || observed.mode() & 0o777 != 0o600
        || observed.nlink() != 1
        || observed.dev() != expected.dev()
        || observed.ino() != expected.ino()
    {
        return Err("inherited update lock is not the exact private transaction inode".into());
    }
    // Even a shared open must be blocked: an originally shared parent lock
    // must never be upgraded into exclusive authority by this validation.
    let shared_probe = probe.try_clone().map_err(|error| error.to_string())?;
    match advisory_lock::FileLock::try_shared(shared_probe) {
        Err(advisory_lock::TryLockError::WouldBlock) => {}
        Err(_) => return Err("cannot establish exclusive update lock ownership".into()),
        Ok(_) => return Err("inherited update transaction is not already exclusive".into()),
    }
    // A separate open must be blocked, while the inherited open description
    // must already own the lock. An unlocked or unrelated descriptor refuses.
    match advisory_lock::FileLock::try_exclusive(probe) {
        Err(advisory_lock::TryLockError::WouldBlock) => {}
        Err(_) => return Err("cannot establish current update lock ownership".into()),
        Ok(_) => return Err("inherited update transaction is not already locked".into()),
    }
    advisory_lock::FileLock::try_exclusive(inherited)
        .map(advisory_lock::FileLock::into_file_preserving_lock)
        .map_err(|_| "inherited descriptor does not own the update transaction".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::inherit;
    use std::{
        fs::OpenOptions,
        os::{
            fd::AsRawFd as _,
            unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
        },
    };

    #[test]
    fn shared_parent_is_refused_without_upgrading_or_unlocking_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shared.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let parent = advisory_lock::FileLock::shared(file).unwrap();
        assert!(inherit(parent.file().as_raw_fd(), &path).is_err());
        let peer =
            advisory_lock::FileLock::try_open(&path, advisory_lock::LockMode::Shared).unwrap();
        assert!(matches!(
            advisory_lock::FileLock::try_open(&path, advisory_lock::LockMode::Exclusive),
            Err(advisory_lock::TryLockError::WouldBlock)
        ));
        drop(peer);
        drop(parent);
        assert!(
            advisory_lock::FileLock::try_open(&path, advisory_lock::LockMode::Exclusive).is_ok()
        );
    }

    #[test]
    fn public_inheritance_preserves_parent_lock_and_refuses_changed_path_authority() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("transaction.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let parent = advisory_lock::FileLock::exclusive(file).unwrap();
        let duplicate = inherit(parent.file().as_raw_fd(), &path).unwrap();
        drop(duplicate);
        let open = || {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap()
        };
        assert!(matches!(
            advisory_lock::FileLock::try_exclusive(open()),
            Err(advisory_lock::TryLockError::WouldBlock)
        ));
        assert!(inherit(-1, &path).is_err());
        assert!(inherit(open().as_raw_fd(), &path).is_err());
        let alias = directory.path().join("linked-parent");
        std::os::unix::fs::symlink(directory.path(), &alias).unwrap();
        assert!(inherit(parent.file().as_raw_fd(), &alias.join("transaction.lock")).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(inherit(parent.file().as_raw_fd(), &path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let hardlink = directory.path().join("hardlink.lock");
        std::fs::hard_link(&path, &hardlink).unwrap();
        assert!(inherit(parent.file().as_raw_fd(), &path).is_err());
        std::fs::remove_file(&hardlink).unwrap();
        std::fs::rename(&path, directory.path().join("captured.lock")).unwrap();
        drop(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap(),
        );
        assert!(inherit(parent.file().as_raw_fd(), &path).is_err());
        drop(parent);
        assert!(advisory_lock::FileLock::try_exclusive(open()).is_ok());
    }
}
