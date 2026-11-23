//! Root controller workspace creation; no source or service authority is minted here.
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::PermissionsExt,
    },
    path::Path,
};

/// Retain the checked, root-controlled worker directory ancestry.
pub(super) fn working_parent() -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    use tilo_files::native::atomic::{Privacy, StorageDirectory};
    let root = File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    StorageDirectory::from_file(root.try_clone()?)?.validate_namespace_control()?;
    let raw = unsafe {
        libc::openat(
            root.as_raw_fd(),
            c"run".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let run = unsafe { File::from_raw_fd(raw) };
    StorageDirectory::from_file(run.try_clone()?)?.validate_namespace_control()?;
    if unsafe { libc::mkdirat(run.as_raw_fd(), c"tilo-retained-workers".as_ptr(), 0o700) } < 0
        && io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST)
    {
        return Err(io::Error::last_os_error());
    }
    let raw = unsafe {
        libc::openat(
            run.as_raw_fd(),
            c"tilo-retained-workers".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let parent = unsafe { File::from_raw_fd(raw) };
    let directory = StorageDirectory::from_file(parent.try_clone()?)?;
    directory.validate_namespace_control()?;
    directory.validate_privacy(Privacy::OwnerOnly)?;
    Ok(parent)
}

/// Create private controller state beneath its already checked worker parent.
/// The caller must retain `working_parent()` throughout this workspace's life.
pub(super) fn temporary_directory(prefix: &str) -> io::Result<tempfile::TempDir> {
    private_directory(prefix, Path::new("/run/tilo-retained-workers"))
}

fn private_directory(prefix: &str, parent: &Path) -> io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(prefix)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn controller_state_is_private_and_does_not_reuse_existing_names() {
        let parent = tempfile::tempdir().unwrap();
        let first = private_directory("worker-", parent.path()).unwrap();
        let second = private_directory("worker-", parent.path()).unwrap();
        assert_ne!(first.path(), second.path());
        for directory in [&first, &second] {
            let facts = std::fs::metadata(directory.path()).unwrap();
            assert!(facts.is_dir());
            assert_eq!(facts.mode() & 0o777, 0o700);
            assert_eq!(facts.uid(), rustix::process::geteuid().as_raw());
        }
    }
}
