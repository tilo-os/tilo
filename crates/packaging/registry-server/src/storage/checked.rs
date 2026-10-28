//! Content-addressed files checked against their names once per file, not
//! once per request.
//!
//! Every object this registry serves by content (payload blobs and chunks,
//! their compressed twins, imported artifacts, thin packages, version
//! indexes) is re-checked against its name before it leaves the server, so a
//! corrupted or swapped file is refused rather than served. The files are
//! immutable: each is written once through an atomic rename and never edited
//! in place. Checking the same unchanged file again on every download only
//! re-hashed (and for a twin, re-decompressed) bytes already known to match,
//! on the I/O threads every other request waits for. Here a file is checked
//! the first time it is served and again only when it is no longer the same
//! file: another inode, length, modification or change time.

use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::StorageError;

/// Which file a path named when it was read: a replaced or edited file has
/// another stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileStamp {
    device: u64,
    inode: u64,
    len: u64,
    modified_ns: i128,
    changed_ns: i128,
}

impl FileStamp {
    pub(crate) fn of(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let ns = |seconds: i64, nanos: i64| {
                i128::from(seconds)
                    .saturating_mul(1_000_000_000)
                    .saturating_add(i128::from(nanos))
            };
            Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                len: metadata.len(),
                modified_ns: ns(metadata.mtime(), metadata.mtime_nsec()),
                changed_ns: ns(metadata.ctime(), metadata.ctime_nsec()),
            }
        }
        #[cfg(not(unix))]
        {
            let ns = |time: std::io::Result<std::time::SystemTime>| {
                time.ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |since| {
                        i128::try_from(since.as_nanos()).unwrap_or(i128::MAX)
                    })
            };
            Self {
                device: 0,
                inode: 0,
                len: metadata.len(),
                modified_ns: ns(metadata.modified()),
                changed_ns: ns(metadata.modified()),
            }
        }
    }

    /// The stamp of the file at `path`, or `None` when there is none.
    ///
    /// # Errors
    /// Returns an error when the file exists but cannot be inspected.
    pub(crate) fn at(path: &Path) -> Result<Option<Self>, StorageError> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(Some(Self::of(&metadata))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StorageError::Io(format!(
                "stat {}: {error}",
                path.display()
            ))),
        }
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// Whether the file last changed at least `settle` ago.
    fn settled(&self, settle: Duration) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                i128::try_from(since.as_nanos()).unwrap_or(i128::MAX)
            });
        now.saturating_sub(self.changed_ns)
            >= i128::try_from(settle.as_nanos()).unwrap_or(i128::MAX)
    }
}

/// The files already checked, by path, with the stamp they had then.
#[derive(Debug)]
pub(crate) struct CheckedFiles {
    checked: Mutex<HashMap<PathBuf, FileStamp>>,
    checks: AtomicU64,
    settle: Duration,
}

impl Default for CheckedFiles {
    fn default() -> Self {
        Self::settling_for(SETTLE)
    }
}

/// How long after its last change a file's stamp is trusted to name its
/// bytes. File times advance in clock ticks, so an edit in place within the
/// tick of an earlier read can leave the stamp unchanged; a file that
/// changed this recently is checked again on every read (as git re-reads a
/// racily clean index entry). Any later change moves the change time past
/// the remembered one, which no user can set back.
const SETTLE: Duration = Duration::from_secs(2);

impl CheckedFiles {
    pub(crate) fn settling_for(settle: Duration) -> Self {
        Self {
            checked: Mutex::default(),
            checks: AtomicU64::new(0),
            settle,
        }
    }

    /// How many times a file's content has been checked.
    pub(crate) fn checks(&self) -> u64 {
        self.checks.load(Ordering::Relaxed)
    }

    /// The bytes of the file at `path`, or `None` when it does not exist.
    /// `admit` sees the file's stamp before it is read (to refuse one too
    /// large to read); `check` sees its bytes, unless this same file has
    /// passed it before.
    pub(crate) fn read(
        &self,
        path: &Path,
        what: &str,
        admit: impl FnOnce(&FileStamp) -> Result<u64, StorageError>,
        check: impl FnOnce(&[u8]) -> Result<(), StorageError>,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(format!("open {what}: {error}"))),
        };
        // The stamp of the opened file, so it names exactly the bytes read
        // even when the path is replaced meanwhile.
        let stamp = FileStamp::of(
            &file
                .metadata()
                .map_err(|error| StorageError::Io(format!("stat {what}: {error}")))?,
        );
        let limit = admit(&stamp)?;
        let mut bytes = Vec::new();
        file.take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| StorageError::Io(format!("read {what}: {error}")))?;
        if bytes.len() as u64 > limit {
            return Err(StorageError::Parse(format!("{what} exceeds its bound")));
        }
        if self.passed(path, &stamp) {
            return Ok(Some(bytes));
        }
        self.checks.fetch_add(1, Ordering::Relaxed);
        check(&bytes)?;
        if stamp.settled(self.settle) {
            self.lock().insert(path.to_path_buf(), stamp);
        }
        Ok(Some(bytes))
    }

    fn passed(&self, path: &Path, stamp: &FileStamp) -> bool {
        self.lock().get(path) == Some(stamp)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, FileStamp>> {
        self.checked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(files: &CheckedFiles, path: &Path) -> Option<Vec<u8>> {
        files
            .read(path, "object", |stamp| Ok(stamp.len()), |_| Ok(()))
            .unwrap()
    }

    #[test]
    fn a_file_is_checked_once_until_it_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("object");
        let files = CheckedFiles::settling_for(Duration::ZERO);
        assert_eq!(read(&files, &path), None);
        assert_eq!(files.checks(), 0, "a missing file is never checked");

        std::fs::write(&path, b"first").unwrap();
        for _ in 0..3 {
            assert_eq!(read(&files, &path).as_deref(), Some(&b"first"[..]));
        }
        assert_eq!(files.checks(), 1);

        // Replaced the way every object is written: a new file renamed over it.
        let next = dir.path().join("object.tmp");
        std::fs::write(&next, b"second").unwrap();
        std::fs::rename(&next, &path).unwrap();
        assert_eq!(read(&files, &path).as_deref(), Some(&b"second"[..]));
        assert_eq!(files.checks(), 2);
    }

    #[test]
    fn a_file_changed_within_the_settle_time_is_checked_on_every_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("object");
        std::fs::write(&path, b"fresh").unwrap();
        let files = CheckedFiles::default();
        for _ in 0..3 {
            read(&files, &path);
        }
        assert_eq!(files.checks(), 3);
    }

    #[test]
    fn a_file_that_fails_its_check_is_checked_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("object");
        std::fs::write(&path, b"bytes").unwrap();
        let files = CheckedFiles::settling_for(Duration::ZERO);
        for _ in 0..2 {
            let refused = files.read(
                &path,
                "object",
                |stamp| Ok(stamp.len()),
                |_| Err(StorageError::Parse("does not match its name".into())),
            );
            assert!(refused.is_err());
        }
        assert_eq!(files.checks(), 2);
    }
}
