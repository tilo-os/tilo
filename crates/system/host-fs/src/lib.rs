//! Portable access to file metadata that only some platforms have:
//! permission bits, symbolic links, extended attributes, no-replace
//! rename, private staging directories and trash management.
//!
//! Apps that preserve or present such metadata (a backup engine, a file
//! manager) call these functions; each one says what it does where the
//! platform has no equivalent, so no app needs its own target checks.

pub mod attributes;
pub mod names;
mod private_dir;
mod rename;
pub mod trash;

pub use private_dir::{create_private_dir, mark_private};
pub use rename::rename_noreplace;

use std::fs::Metadata;
use std::io;
use std::path::Path;

/// Whether this platform has Unix permission bits ([`mode`] returns `Some`).
pub const HAS_MODE: bool = cfg!(unix);

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
