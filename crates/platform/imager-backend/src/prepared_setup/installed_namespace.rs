//! Root-controlled installed metadata, not source admission.
//! Callers must separately authenticate the typed record and its purpose.
use super::invalid;
use std::{
    ffi::{CString, OsStr},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path},
};
use tilo_files::native::atomic::{Privacy, StorageDirectory};
mod lease;
pub use lease::InstalledRecordLease;
const LIMIT: u64 = 132 * 1024;
fn root() -> io::Result<()> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}
pub fn operation(value: &str) -> io::Result<()> {
    if uuid::Uuid::parse_str(value)
        .ok()
        .is_none_or(|id| id.to_string() != value)
    {
        return Err(invalid());
    }
    Ok(())
}
pub fn controlled(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        return Err(invalid());
    }
    StorageDirectory::from_file(file.try_clone()?)?.validate_namespace_control()
}
fn open(parent: &File, name: &OsStr) -> io::Result<File> {
    let name = CString::new(name.as_encoded_bytes()).map_err(io::Error::other)?;
    // SAFETY: the held parent descriptor and NUL-terminated name remain live;
    // flags refuse symlinks and require a directory.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_RDONLY,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a fresh, uniquely owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub fn chain(path: &Path, create: bool) -> io::Result<Vec<File>> {
    chain_with_privacy(path, create, Privacy::OwnerOnly)
}
/// Public signed certificate projection has root-controlled 0755 ancestry;
/// retained private records continue to require the separate OwnerOnly leaf.
pub fn public_certificate_chain(path: &Path) -> io::Result<Vec<File>> {
    chain_with_privacy(path, false, Privacy::Inherit)
}
fn chain_with_privacy(path: &Path, create: bool, privacy: Privacy) -> io::Result<Vec<File>> {
    root()?;
    if !path.is_absolute() {
        return Err(invalid());
    }
    let mut result = vec![
        File::options()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")?,
    ];
    for part in path.components() {
        let Component::Normal(name) = part else {
            if part == Component::RootDir {
                continue;
            }
            return Err(invalid());
        };
        let parent = result.last().ok_or_else(invalid)?;
        controlled(parent)?;
        let next = match open(parent, name) {
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                let encoded = CString::new(name.as_encoded_bytes()).map_err(io::Error::other)?;
                // SAFETY: parent is held and controlled; encoded is a live
                // single-component C string, and creation grants owner-only access.
                if unsafe { libc::mkdirat(parent.as_raw_fd(), encoded.as_ptr(), 0o700) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                parent.sync_all()?;
                open(parent, name)?
            }
            other => other?,
        };
        controlled(&next)?;
        result.push(next);
    }
    StorageDirectory::from_file(result.last().ok_or_else(invalid)?.try_clone()?)?
        .validate_privacy(privacy)?;
    Ok(result)
}
pub fn stamp(file: &File) -> io::Result<(u64, u64, u64, i64, i64)> {
    let m = file.metadata()?;
    if !m.is_file()
        || m.uid() != 0
        || m.mode() & 0o7777 != 0o600
        || m.nlink() != 1
        || m.len() > LIMIT
    {
        return Err(invalid());
    }
    Ok((m.dev(), m.ino(), m.len(), m.ctime(), m.ctime_nsec()))
}
pub fn bytes(file: &File) -> io::Result<Vec<u8>> {
    // A clone shares file position; independent positioned IO avoids mutation
    // of an admission's retained handle during concurrent supervision checks.
    use std::os::unix::fs::FileExt;
    let before = stamp(file)?;
    let mut bytes = vec![0; usize::try_from(before.2).map_err(io::Error::other)?];
    file.read_exact_at(&mut bytes, 0)?;
    if stamp(file)? != before {
        return Err(invalid());
    }
    Ok(bytes)
}
