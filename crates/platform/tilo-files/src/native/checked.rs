//! Descriptor-anchored access to an untrusted native file.
//!
//! Parents are created without following symlinks, then held open across
//! reads, sibling `.lock` advisory locking and atomic writes. Callers hold
//! the lock across reading, merging and publication. This serializes
//! cooperating writers, not hostile replacement of directory entries.
//! On non-Unix targets every operation returns `io::ErrorKind::Unsupported`.

/// Identity comes from the opened file, including its inode so replacements
/// cannot hide behind an equal length and modification time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// Length of the opened file in bytes.
    pub len: u64,
    /// Modification time of the opened file.
    pub modified: std::time::SystemTime,
    device: u64,
    inode: u64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::CheckedCopy;

#[cfg(not(unix))]
use crate::native::lock::FileLock;
#[cfg(not(unix))]
use std::{io, path::Path};

/// Descriptor-anchored native file access, unavailable on this target.
#[cfg(not(unix))]
#[derive(Debug)]
pub struct CheckedCopy;

#[cfg(not(unix))]
impl CheckedCopy {
    /// Open the parent without following links; unsupported on this target.
    ///
    /// # Errors
    /// Always returns [`io::ErrorKind::Unsupported`].
    pub fn open(_path: &Path) -> io::Result<Self> {
        Err(unsupported())
    }

    /// Hold the sibling advisory lock; unsupported on this target.
    ///
    /// # Errors
    /// Always returns [`io::ErrorKind::Unsupported`].
    pub fn lock(&self) -> io::Result<FileLock> {
        Err(unsupported())
    }

    /// Read bytes and the opened file's identity; unsupported on this target.
    ///
    /// # Errors
    /// Always returns [`io::ErrorKind::Unsupported`].
    pub fn read(&self) -> io::Result<(Vec<u8>, Option<Identity>)> {
        Err(unsupported())
    }

    /// Identify the opened regular file; unsupported on this target.
    ///
    /// # Errors
    /// Always returns [`io::ErrorKind::Unsupported`].
    pub fn identity(&self) -> io::Result<Option<Identity>> {
        Err(unsupported())
    }

    /// Atomically publish the supplied bytes; unsupported on this target.
    ///
    /// # Errors
    /// Always returns [`io::ErrorKind::Unsupported`].
    pub fn write(&self, _bytes: &[u8]) -> io::Result<Identity> {
        Err(unsupported())
    }
}

#[cfg(not(unix))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "checked native files require Unix",
    )
}

#[cfg(all(test, not(unix)))]
mod tests {
    #[test]
    fn no_unchecked_platform_fallback() {
        assert_eq!(
            super::CheckedCopy::open(std::path::Path::new("file"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Unsupported
        );
    }
}
