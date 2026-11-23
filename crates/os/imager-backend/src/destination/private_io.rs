//! The unattended installation's private input and output: the recipe and the one-time recovery
//! key's destination. Nothing here follows a link, reads more than its bound, or leaves a credential
//! readable by another account.

use std::io::{self, Read};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use zeroize::Zeroizing;

pub const MAX_RECIPE_BYTES: u64 = 64 * 1024;

pub fn stderr_is_terminal() -> bool {
    unsafe { libc::isatty(libc::STDERR_FILENO) == 1 }
}

/// Read through a duplicate of the inherited descriptor, within [`MAX_RECIPE_BYTES`].
pub fn read_private_fd(fd: RawFd) -> Result<Zeroizing<Vec<u8>>, String> {
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(format!(
            "The unattended recipe descriptor {fd} is not readable: {}",
            io::Error::last_os_error()
        ));
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(duplicated) };
    let metadata = file.metadata().map_err(|error| {
        format!("The unattended recipe descriptor {fd} is not readable: {error}")
    })?;
    if metadata.is_dir() {
        return Err("The unattended recipe descriptor names a directory".into());
    }
    read_bounded(&mut file, "recipe")
}

/// A single regular file of this installer or root, readable by no other account, opened without
/// following a link.
pub fn read_private_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "The unattended recipe path contains a NUL".to_string())?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "The unattended recipe at {} could not be opened without following a link: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file
        .metadata()
        .map_err(|error| format!("The unattended recipe could not be inspected: {error}"))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(
            "The unattended recipe must be a single regular file, not a link or device".into(),
        );
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(
            "The unattended recipe is readable by another account; make it mode 0600".into(),
        );
    }
    let uid = metadata.uid();
    let euid = rustix::process::geteuid().as_raw();
    if uid != euid && uid != 0 {
        return Err("The unattended recipe is not owned by this installer or root".into());
    }
    read_bounded(&mut file, "recipe")
}

fn read_bounded(file: &mut std::fs::File, what: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_RECIPE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("The {what} could not be read: {error}"))?;
    if bytes.len() as u64 > MAX_RECIPE_BYTES {
        return Err(format!("The {what} is larger than the accepted bound"));
    }
    Ok(bytes)
}

/// Through a duplicate of the inherited descriptor.
pub fn writable_fd(fd: RawFd) -> Result<std::fs::File, String> {
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(format!(
            "The recovery-key descriptor {fd} is not writable: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(duplicated) })
}

pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Never a path that exists already, never through a final symlink.
pub fn open_private_output(path: &Path) -> Result<std::fs::File, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_info = std::fs::symlink_metadata(parent).map_err(|error| {
        format!(
            "The recovery-key directory {} is unavailable: {error}",
            parent.display()
        )
    })?;
    if !parent_info.is_dir() || parent_info.file_type().is_symlink() {
        return Err(format!(
            "The recovery-key directory {} is not a plain directory",
            parent.display()
        ));
    }
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "The recovery-key path contains a NUL".to_string())?;
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(format!(
            "The recovery-key file {} could not be created privately: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Or the temporary directory where the session has none.
pub fn session_runtime_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

pub fn is_device_path(name: &str) -> bool {
    name.starts_with("/dev/")
}
