//! Descriptor-anchored access to an untrusted native file. Parents are created without
//! following symlinks, then held open across reads, sibling `.lock` advisory locking
//! and atomic writes, and callers hold the lock across reading, merging and
//! publication: that serializes cooperating writers, not hostile replacement of
//! directory entries. On non-Unix targets every operation returns
//! `io::ErrorKind::Unsupported`.

/// Identity comes from the opened file, including its inode, so a replacement cannot hide behind an
/// equal length and modification time. [`Identity::of_path`] identifies what a path names now; a caller
/// that records an identity to compare against later stores it with the `json` feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "json", derive(serde::Serialize, serde::Deserialize))]
pub struct Identity {
    pub len: u64,
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

#[cfg(not(unix))]
#[derive(Debug)]
pub struct CheckedCopy;

#[cfg(not(unix))]
impl CheckedCopy {
    pub fn open(_path: &Path) -> io::Result<Self> {
        Err(unsupported())
    }

    pub fn lock(&self) -> io::Result<FileLock> {
        Err(unsupported())
    }

    pub fn read(&self) -> io::Result<(Vec<u8>, Option<Identity>)> {
        Err(unsupported())
    }

    pub fn identity(&self) -> io::Result<Option<Identity>> {
        Err(unsupported())
    }

    pub fn write(&self, _bytes: &[u8]) -> io::Result<Identity> {
        Err(unsupported())
    }
}

#[cfg(not(unix))]
impl Identity {
    pub fn of_path(_path: &Path) -> io::Result<Option<Self>> {
        Err(unsupported())
    }

    pub fn of_file(_file: &std::fs::File) -> io::Result<Self> {
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
