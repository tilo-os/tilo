//! Portable access to file metadata that only some platforms have:
//! permission bits, symbolic links, extended attributes, no-replace
//! rename, private staging directories, trash management and the capacity
//! and free space of a filesystem.
//!
//! Apps that preserve or present such metadata (a backup engine, a file
//! manager) call these functions; each one says what it does where the
//! platform has no equivalent, so no app needs its own target checks.

pub mod attributes;
pub mod names;
mod private_dir;
mod rename;
mod retained_object;
mod space;
pub mod trash;

pub use private_dir::{create_private_dir, mark_private};
pub use rename::rename_noreplace;
pub use retained_object::{ObservedObject, RetainedObject};
pub use space::{Space, space};

use std::fs::Metadata;
use std::io;
use std::path::Path;

/// The Unix permission and file-type bits of `metadata` (`st_mode`), or
/// `None` on platforms without them.
#[inline]
#[must_use]
pub fn mode(metadata: &Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        Some(metadata.permissions().mode())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

/// Apply Unix permission bits to `path`. A no-op on platforms without them.
///
/// # Errors
///
/// Returns the permission change's error.
#[inline]
pub fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Whether [`symlink`] can create symbolic links on this platform.
pub const HAS_SYMLINKS: bool = cfg!(unix);

/// Create a symbolic link at `link` pointing to `target`.
///
/// # Errors
///
/// Returns the creation error, or [`io::ErrorKind::Unsupported`] where
/// [`HAS_SYMLINKS`] is `false`.
#[inline]
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(not(unix))]
    {
        let _ = (target, link);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symbolic links are not supported on this platform",
        ))
    }
}

/// Create a symlink containing `target` beneath an already-open directory.
/// The target is data, including absolute paths; it is never opened.
///
/// # Errors
/// Refuses a non-leaf name, NUL bytes, unsupported hosts, or host I/O errors.
pub fn symlink_at(
    target: &Path,
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
) -> io::Result<()> {
    let path = Path::new(name);
    if path.components().count() != 1
        || !matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "symlink name must be a leaf",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::ffi::OsStrExt as _;
        let target = std::ffi::CString::new(target.as_os_str().as_bytes())?;
        let name = std::ffi::CString::new(name.as_bytes())?;
        // SAFETY: live retained directory descriptor and two terminated byte strings.
        if unsafe { libc::symlinkat(target.as_ptr(), directory.as_raw_fd(), name.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (target, directory, name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "copying symbolic links is not supported on this platform",
        ))
    }
}
