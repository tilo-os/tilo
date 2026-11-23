//! Whether a file changed since it was last read: a follower keeps a file's stamp and reads the file
//! again when the stamp moves.

use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// How old a modification time must be before a later write, which stamps the current time, can no
/// longer share it: a file written twice within this time may keep one stamp.
pub const SETTLE: Duration = Duration::from_secs(2);

/// A file's contents as far as a writer can change them: a rename over it gives a new file id, an
/// in-place rewrite a new length or modification time, and a permission change or a modification
/// time set back a new change time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileStamp {
    volume: u64,
    file: u128,
    len: u64,
    modified: SystemTime,
    changed: i128,
}

impl FileStamp {
    /// The stamp of the file or directory `path` names, following links; `Ok(None)` while nothing
    /// is there.
    ///
    /// # Errors
    /// What is there cannot be examined.
    pub fn of(path: &Path) -> io::Result<Option<Self>> {
        match platform::of(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            stamp => stamp.map(Some),
        }
    }

    /// Old enough that a later write cannot share its modification time ([`SETTLE`]).
    #[must_use]
    pub fn settled(self) -> bool {
        SystemTime::now()
            .duration_since(self.modified)
            .is_ok_and(|age| age >= SETTLE)
    }

    /// When it is [`Self::settled`].
    #[must_use]
    pub fn settles_at(self) -> SystemTime {
        self.modified.checked_add(SETTLE).unwrap_or(self.modified)
    }
}

#[cfg(unix)]
mod platform {
    use super::{FileStamp, Path, io};
    use std::os::unix::fs::MetadataExt as _;

    pub fn of(path: &Path) -> io::Result<FileStamp> {
        let metadata = std::fs::metadata(path)?;
        Ok(FileStamp {
            volume: metadata.dev(),
            file: u128::from(metadata.ino()),
            len: metadata.len(),
            modified: metadata.modified()?,
            changed: i128::from(metadata.ctime())
                .saturating_mul(1_000_000_000)
                .saturating_add(i128::from(metadata.ctime_nsec())),
        })
    }
}

/// The volume and the 128-bit id NTFS and ReFS give a file, and its change time, from a handle
/// opened only to read attributes, so a file another process holds open is stamped all the same.
#[cfg(windows)]
mod platform {
    use super::{FileStamp, Path, io};
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileBasicInfo,
        FileIdInfo, GetFileInformationByHandleEx,
    };

    fn information<T>(file: &std::fs::File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
        // SAFETY: T is one of the plain FILE_*_INFO records, valid zeroed.
        let mut information: T = unsafe { std::mem::zeroed() };
        // SAFETY: a live handle and a buffer of exactly T's size for the class that fills a T.
        let read = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                class,
                (&raw mut information).cast(),
                u32::try_from(size_of::<T>()).unwrap_or(u32::MAX),
            )
        };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(information)
    }

    pub fn of(path: &Path) -> io::Result<FileStamp> {
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        let id: FILE_ID_INFO = information(&file, FileIdInfo)?;
        let basic: FILE_BASIC_INFO = information(&file, FileBasicInfo)?;
        let metadata = file.metadata()?;
        Ok(FileStamp {
            volume: id.VolumeSerialNumber,
            file: u128::from_le_bytes(id.FileId.Identifier),
            len: metadata.len(),
            modified: metadata.modified()?,
            changed: i128::from(basic.ChangeTime),
        })
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::{FileStamp, Path, io};

    pub fn of(_path: &Path) -> io::Result<FileStamp> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rewrite_or_a_replacement_moves_the_stamp_and_a_read_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fleet.json");
        assert_eq!(FileStamp::of(&path).unwrap(), None);
        std::fs::write(&path, b"one").unwrap();
        let first = FileStamp::of(&path).unwrap().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        assert_eq!(FileStamp::of(&path).unwrap(), Some(first));

        std::fs::write(&path, b"three").unwrap();
        let rewritten = FileStamp::of(&path).unwrap().unwrap();
        assert_ne!(rewritten, first);

        let other = dir.path().join("fleet.json.tmp");
        std::fs::write(&other, b"three").unwrap();
        std::fs::rename(&other, &path).unwrap();
        assert_ne!(FileStamp::of(&path).unwrap().unwrap(), rewritten);
        assert!(FileStamp::of(dir.path()).unwrap().is_some());
    }
}
