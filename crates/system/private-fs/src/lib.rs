//! Private identity files: validate the opened object, never just its path.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "test assertions exercise real filesystem behavior"
    )
)]
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(windows)]
#[path = "private_fs_windows.rs"]
mod platform;

#[cfg(unix)]
mod platform {
    use super::{File, OpenOptions, Path, io};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

    pub fn directory(path: &Path) -> io::Result<()> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity directory is not user-owned",
            ));
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }

    pub fn open(path: &Path, write: bool, create: bool, exclusive: bool) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .create(create)
            .create_new(exclusive)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity file is not private and user-owned",
            ));
        }
        Ok(file)
    }
}

/// Create or restrict a user-owned private directory.
///
/// # Errors
/// Returns filesystem errors or rejects a foreign owner or a directory link.
pub fn directory(path: &Path) -> io::Result<()> {
    platform::directory(path)
}
/// Open a private regular file, validating the opened object's owner and access.
///
/// # Errors
/// Returns filesystem errors or rejects links, foreign owners and public access.
pub fn read(path: &Path) -> io::Result<File> {
    platform::open(path, false, false, false)
}
/// Exclusively create a file with user-only access from the moment of creation.
///
/// # Errors
/// Returns filesystem errors, including `AlreadyExists` without overwriting.
pub fn create_new(path: &Path) -> io::Result<File> {
    platform::open(path, true, false, true)
}
/// Open or create a private file for read/write access; the caller acquires locks.
///
/// # Errors
/// Returns filesystem errors or rejects an existing non-private file or link.
pub fn lock_file(path: &Path) -> io::Result<File> {
    platform::open(path, true, true, false)
}
/// Validate an existing file before truncating it, or create a new private file.
///
/// # Errors
/// Returns validation or I/O errors. Rejected existing files are not truncated.
pub fn rewrite(path: &Path) -> io::Result<File> {
    let file = lock_file(path)?;
    // Validate before truncating an existing file.
    file.set_len(0)?;
    Ok(file)
}
/// Flush a directory after publishing a private file.
///
/// # Errors
/// Returns directory open or flush errors; durability failures are not ignored.
pub fn sync_directory(path: &Path) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.write(true).custom_flags(0x0200_0000 | 0x0020_0000);
    }
    options.open(path)?.sync_all()
}

/// Atomically replace a private file using a private, exclusively created sibling.
/// Existing destinations must pass the same checks as [`read`].
///
/// # Errors
/// Returns validation, write, flush or rename errors. A failure after the final
/// rename may mean publication succeeded but its directory flush did not.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    publish(path, bytes, true).map(|_| ())
}

/// Publish a complete private file only if its final name is still absent.
/// Returns `false` when another writer already owns the name, without replacing it.
///
/// # Errors
/// Returns creation, write, flush or hard-link errors. Filesystems must support
/// same-directory hard links for the no-clobber publication guarantee.
pub fn create_atomic(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    publish(path, bytes, false)
}

fn publish(path: &Path, bytes: &[u8], replace: bool) -> io::Result<bool> {
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private file has no filename",
        ));
    }
    // The temp file is opened through [`create_new`], so it is validated as
    // private and user-owned before any secret byte is written to it.
    if replace {
        match read(path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        atomic_file::write_with_temp_opener(path, bytes, create_new).map(|()| true)
    } else {
        atomic_file::create_with_temp_opener(path, bytes, create_new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn atomic_replacement_stays_private_and_leaves_no_temporary_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("key.bin");
        write_atomic(&path, b"old secret").unwrap();
        write_atomic(&path, b"new secret").unwrap();
        let mut bytes = Vec::new();
        read(&path).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"new secret");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn private_files_round_trip_and_creation_never_overwrites() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("identity");
        directory(&root).unwrap();
        let path = root.join("secret");
        let mut file = create_new(&path).unwrap();
        file.write_all(b"private identity").unwrap();
        file.sync_all().unwrap();
        drop(file);
        sync_directory(&root).unwrap();
        assert_eq!(
            create_new(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let mut bytes = Vec::new();
        read(&path).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"private identity");
        rewrite(&path).unwrap().write_all(b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn directories_and_files_cannot_substitute_for_one_another() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("identity");
        directory(&root).unwrap();
        assert!(read(&root).is_err());
        let file = root.join("file");
        create_new(&file).unwrap();
        assert!(directory(&file).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn junction_cannot_redirect_identity_directory() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let link = temporary.path().join("junction");
        std::fs::create_dir(&target).unwrap();
        let sentinel = target.join("untouched");
        std::fs::write(&sentinel, b"outside").unwrap();
        let result = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(result.status.success(), "{:?}", result);
        assert_eq!(
            directory(&link).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"outside");
        // Remove only the junction itself, never recursively traverse it.
        std::fs::remove_dir(&link).unwrap();
        assert!(target.is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn public_read_access_is_rejected_before_truncation() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("identity");
        directory(&root).unwrap();
        let path = root.join("secret");
        create_new(&path)
            .unwrap()
            .write_all(b"must survive")
            .unwrap();
        // icacls is the Windows ACL editor; grant the well-known Everyone SID
        // explicitly so this fixture is independent of OS display language.
        let result = std::process::Command::new("icacls.exe")
            .arg(&path)
            .args(["/grant", "*S-1-1-0:(R)"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{:?}", result);
        assert_eq!(
            read(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            rewrite(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            write_atomic(&path, b"replacement").unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"must survive");
    }

    #[cfg(unix)]
    #[test]
    fn public_read_access_is_rejected_before_truncation() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("secret");
        create_new(&path)
            .unwrap()
            .write_all(b"must survive")
            .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read(&path).is_err());
        assert!(rewrite(&path).is_err());
        assert!(write_atomic(&path, b"replacement").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"must survive");
    }

    #[test]
    fn atomic_creation_never_replaces_an_existing_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("broker-identity");
        assert!(create_atomic(&path, b"first").unwrap());
        assert!(!create_atomic(&path, b"second").unwrap());
        let mut bytes = Vec::new();
        read(&path).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"first");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
