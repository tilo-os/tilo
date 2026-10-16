//! Private identity files: validate the opened object, never just its path.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(windows)]
#[path = "private/windows.rs"]
mod platform;

/// Targets without an owner-checked filesystem, such as the browser, have no
/// private files: every request is refused rather than weakened.
#[cfg(not(any(unix, windows)))]
mod platform {
    use super::{File, Path, io};

    pub fn directory(_path: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    pub fn open(_path: &Path, _write: bool, _create: bool, _exclusive: bool) -> io::Result<File> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(unix)]
mod platform {
    use super::{File, OpenOptions, Path, io};
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    pub fn directory(path: &Path) -> io::Result<()> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || !super::owned_by_me(&metadata) {
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
        if !metadata.is_file() || !super::private_to_me(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity file is not private and user-owned",
            ));
        }
        Ok(file)
    }
}

/// Make `options` create files that only their owner can read or write
/// (mode `0600` on Unix; other platforms keep their default access).
///
/// Unlike [`create_new`] this adds no ownership validation, so it costs no
/// extra system call.
#[inline]
pub fn owner_only(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

/// Restrict an existing directory to its owner (mode `0700` on Unix; a no-op
/// on platforms without permission bits).
///
/// # Errors
/// Returns the permission change's error.
pub fn restrict_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Whether `metadata` grants no access to group or other users. Always `true`
/// on platforms without Unix permission bits.
#[inline]
#[must_use]
pub fn is_owner_only(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        #[expect(
            clippy::verbose_bit_mask,
            reason = "the group and other permission bits read clearer as a mask"
        )]
        let owner_only = metadata.permissions().mode() & 0o077 == 0;
        owner_only
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

/// Whether this process's effective user owns what `metadata` describes.
/// Always `true` on platforms without Unix ownership.
#[inline]
#[must_use]
pub fn owned_by_me(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        metadata.uid() == rustix::process::geteuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

/// Whether what `metadata` describes is [owned by this user](owned_by_me) and
/// [closed to everyone else](is_owner_only).
#[inline]
#[must_use]
pub fn private_to_me(metadata: &std::fs::Metadata) -> bool {
    owned_by_me(metadata) && is_owner_only(metadata)
}

/// Open an existing regular file the current user owns, for reading at most
/// `max_len` bytes, and return it with its length.
///
/// A final symbolic link is never followed and the opened object must be the
/// one inspected, so a rename cannot redirect the read. A file that grants
/// access beyond its owner is narrowed to owner-only (`0600` on Unix) before
/// it is returned. Other platforms check only the file type and length.
///
/// # Errors
/// Returns filesystem errors, or rejects links, special files, foreign owners,
/// oversized files, and a file that changed while opening.
pub fn open_owned_for_read(path: &Path, max_len: u64) -> io::Result<(File, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() {
            return Err(io::Error::other("target is not a regular file"));
        }
        if !owned_by_me(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file is not owned by the current user",
            ));
        }
        if metadata.len() > max_len {
            return Err(io::Error::other("file exceeds size limit"));
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let opened = file.metadata()?;
        if !opened.file_type().is_file()
            || opened.len() > max_len
            || !owned_by_me(&opened)
            || opened.dev() != metadata.dev()
            || opened.ino() != metadata.ino()
        {
            return Err(io::Error::other("target changed while opening"));
        }
        if opened.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "could not enforce owner-only file permissions",
                ));
            }
        }
        Ok((file, opened.len()))
    }
    #[cfg(not(unix))]
    {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() {
            return Err(io::Error::other("target is not a regular file"));
        }
        if metadata.len() > max_len {
            return Err(io::Error::other("file exceeds size limit"));
        }
        let file = File::open(path)?;
        let opened = file.metadata()?;
        if !opened.file_type().is_file() || opened.len() > max_len {
            return Err(io::Error::other("target changed while opening"));
        }
        Ok((file, opened.len()))
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
        super::atomic::write_with_temp_opener(path, bytes, create_new).map(|()| true)
    } else {
        super::atomic::create_with_temp_opener(path, bytes, create_new)
    }
}

/// Bind a Unix socket at `path` that only its owner can connect to (mode
/// `0600`), replacing a socket left there by a previous run. Any other kind
/// of file at `path` is never removed. The parent directory must exist.
///
/// # Errors
/// [`io::ErrorKind::AlreadyExists`] when `path` is not a socket, and the
/// removal's, bind's or permission change's error otherwise.
#[cfg(unix)]
pub fn bind_socket(path: &Path) -> io::Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};
    use std::os::unix::net::UnixListener;

    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_socket() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Ok(_) => match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(listener)
}

/// The one listener systemd socket activation passed this process, or `None`
/// when it was not socket-activated. The descriptor is marked close-on-exec
/// and the `LISTEN_*` variables are cleared, so no child inherits the endpoint
/// or the activation. A listener bound to a path other than `expected` is
/// refused, so an unrelated inherited descriptor never becomes the endpoint;
/// one without a pathname is accepted, as systemd may pass it unnamed.
///
/// # Errors
///
/// Returns an error when the activation does not pass exactly one open
/// descriptor, the descriptor is bound elsewhere, or it cannot be marked
/// close-on-exec.
///
/// # Safety
///
/// Clears environment variables: no other thread may read or write the
/// environment while this runs.
#[cfg(unix)]
#[allow(
    clippy::disallowed_methods,
    reason = "systemd socket activation: LISTEN_* must not leak to children; the caller guarantees no other thread touches the environment"
)]
pub unsafe fn activated_socket(
    expected: &Path,
) -> io::Result<Option<std::os::unix::net::UnixListener>> {
    use std::os::fd::FromRawFd as _;

    const SYSTEMD_LISTEN_FD: libc::c_int = 3;
    let pid_matches = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(std::process::id());
    if !pid_matches {
        return Ok(None);
    }
    if std::env::var("LISTEN_FDS").as_deref() != Ok("1") {
        return Err(io::Error::other(format!(
            "systemd must pass exactly one listener for {}",
            expected.display()
        )));
    }
    // SAFETY: fcntl only checks whether the scalar descriptor is open.
    if unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_GETFD) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the LISTEN_PID/LISTEN_FDS contract hands fd 3 to this process
    // exactly once, and the variables are cleared below.
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(SYSTEMD_LISTEN_FD) };
    if let Some(actual) = listener.local_addr()?.as_pathname()
        && actual != expected
    {
        return Err(io::Error::other(format!(
            "systemd passed {} where {} was expected",
            actual.display(),
            expected.display()
        )));
    }
    // systemd passes activation descriptors without close-on-exec.
    // SAFETY: fcntl on an open descriptor with integer arguments.
    if unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the caller guarantees no concurrent environment access.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    Ok(Some(listener))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn private_files_are_created_and_replaced_atomically_without_substitution() {
        // Phase `atomic_replacement_stays_private_and_leaves_no_temporary_files`.
        {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("key.bin");
            write_atomic(&path, b"old secret").unwrap();
            write_atomic(&path, b"new secret").unwrap();
            let mut bytes = Vec::new();
            read(&path).unwrap().read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"new secret");
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        }
        // Phase `private_files_round_trip_and_creation_never_overwrites`.
        {
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
        // Phase `directories_and_files_cannot_substitute_for_one_another`.
        {
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("identity");
            directory(&root).unwrap();
            assert!(read(&root).is_err());
            let file = root.join("file");
            create_new(&file).unwrap();
            assert!(directory(&file).is_err());
        }
        // Phase `atomic_creation_never_replaces_an_existing_file`.
        {
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

    #[cfg(unix)]
    #[test]
    fn socket_binds_owner_only_and_replaces_only_a_socket() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("service.sock");

        let previous = bind_socket(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        drop(previous);
        let _replacement = bind_socket(&path).unwrap();

        let file = root.path().join("not-a-socket");
        std::fs::write(&file, b"keep").unwrap();
        assert_eq!(
            bind_socket(&file).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&file).unwrap(), b"keep");
    }
}
