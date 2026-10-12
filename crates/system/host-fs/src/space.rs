//! How big the filesystem holding a path is and how much of it is free.

use std::io;
use std::path::Path;

/// The size of the filesystem holding a path, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Space {
    /// Everything the filesystem holds.
    pub capacity: u64,
    /// Free, including blocks reserved for the administrator.
    pub free: u64,
    /// Free to an unprivileged user: what a person can still store.
    pub available: u64,
}

impl Space {
    /// What is in use: capacity less everything free.
    #[must_use]
    pub const fn used(&self) -> u64 {
        self.capacity.saturating_sub(self.free)
    }
}

/// The capacity and free space of the filesystem holding `path`.
///
/// # Errors
///
/// Returns the host's error; `Unsupported` where the host cannot say.
pub fn space(path: &Path) -> io::Result<Space> {
    imp::space(path)
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::Space;

    #[allow(
        clippy::unnecessary_cast,
        reason = "statvfs field widths differ between hosts; u64 holds every one"
    )]
    pub fn space(path: &Path) -> io::Result<Space> {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
        // SAFETY: `path` is a live NUL-terminated buffer and `stats` points
        // at writable storage the size of a `statvfs`.
        let result = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: statvfs succeeded, so it initialised every field.
        let stats = unsafe { stats.assume_init() };
        let unit = (stats.f_frsize as u64).max(1);
        Ok(Space {
            capacity: (stats.f_blocks as u64).saturating_mul(unit),
            free: (stats.f_bfree as u64).saturating_mul(unit),
            available: (stats.f_bavail as u64).saturating_mul(unit),
        })
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::path::Path;

    use super::Space;

    pub fn space(path: &Path) -> io::Result<Space> {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem capacity is unavailable on this host",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filesystem_holding_a_directory_has_a_size() {
        let dir = std::env::temp_dir();
        let space = space(&dir).expect("the temporary directory's filesystem");
        assert!(space.capacity > 0, "{space:?}");
        assert!(space.free <= space.capacity, "{space:?}");
        assert!(space.available <= space.free, "{space:?}");
        assert_eq!(space.used(), space.capacity - space.free);
    }

    #[test]
    fn a_missing_path_is_an_error() {
        assert!(space(Path::new("/nonexistent/tilo/host-fs/space")).is_err());
    }
}
