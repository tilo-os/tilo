//! Private identity files: validate the opened object, never just its path.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(windows)]
#[path = "private/windows.rs"]
mod platform;

/// Targets without an owner-checked filesystem, such as the browser, have no private files: every
/// request is refused rather than weakened.
#[cfg(not(any(unix, windows)))]
mod platform {
    use super::{Access, File, Path, io};

    pub fn directory(_path: &Path) -> io::Result<File> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    pub fn open(
        _path: &Path,
        _access: Access,
        _create: bool,
        _exclusive: bool,
    ) -> io::Result<File> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// What a private file is opened for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Write,
    /// Read, and write at its end only.
    Append,
}

#[cfg(unix)]
mod platform {
    use super::{Access, File, OpenOptions, Path, io};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

    pub fn directory(path: &Path) -> io::Result<File> {
        let refused = || {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a private directory must be this user's own and not a symlink",
            )
        };
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || !super::owned_by_me(&metadata) {
            return Err(refused());
        }
        restrict(path, &metadata)?;
        // What was restricted by its name is checked again as the directory opened.
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let opened = directory.metadata()?;
        if !opened.is_dir() || !super::private_to_me(&opened) {
            return Err(refused());
        }
        Ok(directory)
    }

    /// Mode `0700`, set only when the directory has another: a chmod that changes nothing still tells
    /// every watcher of the directory that its attributes changed, so a caller that only reads must not
    /// make one.
    pub fn restrict(path: &Path, metadata: &std::fs::Metadata) -> io::Result<()> {
        if metadata.permissions().mode() & 0o7777 == 0o700 {
            return Ok(());
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }

    pub fn open(path: &Path, access: Access, create: bool, exclusive: bool) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(access == Access::Write)
            .append(access == Access::Append)
            .create(create)
            .create_new(exclusive)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let metadata = file.metadata()?;
        // A second link is a name for the same bytes outside the directory the caller checked.
        if !metadata.is_file() || !super::private_to_me(&metadata) || metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{}: a private file must be this user's own, owner-only and singly linked",
                    path.display()
                ),
            ));
        }
        Ok(file)
    }
}

/// Make `options` create files only their owner can read or write (mode `0600` on Unix; other platforms
/// keep their default access).
#[inline]
pub fn owner_only(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

/// Mode `0700` on Unix, a no-op without permission bits; a directory already at that mode is left
/// untouched, so restricting it is not a change its watchers see.
pub fn restrict_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        platform::restrict(path, &std::fs::metadata(path)?)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Always `true` on platforms without Unix permission bits.
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

#[inline]
#[must_use]
pub fn private_to_me(metadata: &std::fs::Metadata) -> bool {
    owned_by_me(metadata) && is_owner_only(metadata)
}

/// Opens an existing regular file the current user owns, for reading at most `max_len` bytes, and
/// returns it with its length. A file that grew, changed owner or was replaced while it was opened is
/// refused.
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

/// Creates or restricts a user-owned private directory.
pub fn directory(path: &Path) -> io::Result<()> {
    open_directory(path).map(drop)
}
/// [`directory`], and the directory it made private, opened without following a link and checked
/// as opened: a handle to flush after renaming into it.
pub fn open_directory(path: &Path) -> io::Result<File> {
    platform::directory(path)
}
/// Opens a private regular file, validating the opened object's owner and access.
pub fn read(path: &Path) -> io::Result<File> {
    platform::open(path, Access::Read, false, false)
}
/// [`read`], and every write lands at the file's end, wherever the file was cut back to.
pub fn append(path: &Path) -> io::Result<File> {
    platform::open(path, Access::Append, false, false)
}
/// [`create_new`], written as [`append`] writes.
pub fn create_new_append(path: &Path) -> io::Result<File> {
    platform::open(path, Access::Append, false, true)
}
/// Unix ciphertext may retain inherited read permissions; this is only for authenticated ciphertext,
/// never plaintext credentials or encryption keys. Other platforms keep the canonical private-file ACL
/// requirements.
#[cfg(unix)]
pub fn read_ciphertext(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "encrypted file is not a user-owned regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
pub fn read_ciphertext(path: &Path) -> io::Result<File> {
    read(path)
}

/// User-only access from the moment of creation.
pub fn create_new(path: &Path) -> io::Result<File> {
    platform::open(path, Access::Write, false, true)
}
/// Opens or creates a private file for read and write; the caller acquires locks.
pub fn lock_file(path: &Path) -> io::Result<File> {
    platform::open(path, Access::Write, true, false)
}
/// Validates an existing file before truncating it, or creates a new private file.
pub fn rewrite(path: &Path) -> io::Result<File> {
    let file = lock_file(path)?;
    file.set_len(0)?;
    Ok(file)
}

/// Uses a private, exclusively created sibling; an existing destination must pass the same checks as
/// [`read`].
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    publish(path, bytes, true).map(|_| ())
}

/// Publishes a complete private file only if its final name is still absent; `false` when another
/// writer owns the name.
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
    // The temp file is opened through [`create_new`], so it is validated as private and user-owned
    // before any secret byte is written into it.
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

/// As the Unix [`bind_socket`] below: a named pipe is already this account's alone.
#[cfg(windows)]
pub fn bind_socket(path: &Path) -> io::Result<super::socket::Listener> {
    super::socket::bind(path)
}

/// Binds a local socket at `path` that only its owner can connect to, replacing a socket left there
/// by a previous run: on Unix a socket file of mode `0600`; on Windows a named pipe, whose DACL and
/// peer check keep it to this account and which leaves nothing behind to replace.
#[cfg(unix)]
pub fn bind_socket(path: &Path) -> io::Result<super::socket::Listener> {
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};

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
    let listener = super::socket::bind(path)?;
    if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(listener)
}

/// The one listener systemd socket activation passed this process, or `None` when it was not
/// socket-activated. The descriptor is marked close-on-exec and the `LISTEN_*` variables are cleared,
/// so no child inherits the endpoint or the activation; a listener bound to a path other than
/// `expected` is refused, and one without a pathname is accepted, as systemd may pass it unnamed.
///
/// # Safety
///
/// Clears environment variables, so no other thread may read or write the environment while this
/// runs.
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
    // SAFETY: the LISTEN_PID/LISTEN_FDS contract hands fd 3 to this process exactly once, and the
    // variables are cleared below.
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
    // systemd passes activation descriptors without close-on-exec. SAFETY: fcntl on an open descriptor
    // with integer arguments.
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
