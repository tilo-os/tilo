//! Destination namespace protection, separate from read-only source access.
//!
//! The guarantee excludes hostile processes running as the current account or
//! an administrator: those principals can alter final output regardless of the
//! copy protocol. Other accounts must not be able to replace operation-owned
//! names between identity checks and mkdir/link/unlink. Unknown security or
//! filesystem semantics are a refusal, not an assumption of private access.

use std::io;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{check_directory, check_private_file, create_directory};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{check_directory, check_private_file, create_directory};

#[cfg(all(windows, feature = "test-support"))]
pub use windows::create_acl_fixture;

/// # Errors
/// Always refuses platforms without a supported namespace-security adapter.
#[cfg(not(any(unix, windows)))]
pub fn check_directory(
    _file: &std::fs::File,
    _child: Option<&cap_primitives::fs::Metadata>,
    _private: bool,
) -> io::Result<()> {
    Err(refusal(
        "destination namespace protection is unavailable on this platform",
    ))
}

/// # Errors
/// Always refuses platforms without a supported namespace-security adapter.
#[cfg(not(any(unix, windows)))]
pub fn check_private_file(_file: &std::fs::File) -> io::Result<()> {
    Err(refusal(
        "destination namespace protection is unavailable on this platform",
    ))
}

/// # Errors
/// Always refuses platforms without a supported namespace-security adapter.
#[cfg(not(any(unix, windows)))]
pub fn create_directory(_parent: &std::fs::File, _name: &std::ffi::OsStr) -> io::Result<()> {
    Err(refusal(
        "destination namespace protection is unavailable on this platform",
    ))
}

fn refusal(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

/// Read one already-granted private file, up to `limit` bytes.
///
/// The parent is pinned, the name is not followed and the open does not block,
/// so a redirect, a device or a FIFO is refused instead of followed or waited
/// on. A larger file is refused, never truncated into a shorter artifact.
///
/// # Errors
///
/// When the parent or file is missing, not private, redirected or oversized.
pub fn read_private_file(path: &std::path::Path, limit: usize) -> io::Result<Vec<u8>> {
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    use std::io::Read as _;

    use crate::migration_directory::PinnedDirectory;

    let parent = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| io::Error::other("the file needs a parent"))?,
    )?;
    check_private_file(&parent.file)?;
    parent.validate_destination()?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("the file needs a name"))?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut file = cap_primitives::fs::open(&parent.file, std::path::Path::new(name), &options)?;
    check_private_file(&file)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(io::Error::other("the file exceeds bounds or is not a file"));
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    parent.validate()?;
    Ok(bytes)
}
