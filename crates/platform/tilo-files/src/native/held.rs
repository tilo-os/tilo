//! A file its writer stands behind for as long as it lives: published with
//! the writer's lock on it, and read as current only while that lock is held.
//!
//! The writer keeps the [`Held`] it got from [`publish`]. When the writer
//! exits, crashes or lets go (a failed replacement drops it), the kernel
//! releases the lock with it, so a reader learns that the contents are no
//! longer vouched for at once: nobody renews anything on a clock.
//!
//! The lock is an exclusive `flock(2)` on the published file, taken before
//! the file is renamed into place, so the name never names a file of a live
//! writer that is not locked yet. A reader tries a *shared* lock: refused
//! means the writer holds it. Granted means nobody does, unless the name has
//! meanwhile been replaced by a newer file, which the reader then reads.
//! Readers never conflict with one another, and a reader can neither take
//! the writer's place nor block it: every publication locks a new file.

use std::fs::File;
use std::io;
use std::path::Path;

use super::lock::{FileLock, LockMode, TryLockError};

/// How many times [`read`] follows a replacement before giving up. A writer
/// replaces the file only when its contents change.
const MAX_REPLACEMENTS_FOLLOWED: usize = 8;

/// The writer's hold on a file it published with [`publish`]; dropping it
/// withdraws the file's contents from readers.
#[derive(Debug)]
#[must_use = "the published file is current only while this is held"]
pub struct Held {
    _lock: FileLock,
}

/// Replace `path` with `bytes` (owner and `mode` on Unix) and hold it.
///
/// Like [`super::atomic::write_regenerable_with_mode`], the file is neither
/// synced nor its directory: this is for a runtime directory, which a crash
/// discards along with the writer.
///
/// # Errors
/// Returns the creation, lock, write or rename error; `path` then still
/// names what it did before, and that file's own [`Held`] decides it.
pub fn publish(path: &Path, bytes: &[u8], mode: u32) -> io::Result<Held> {
    let mut held = None;
    super::atomic::publish(
        path,
        bytes,
        |tmp| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, mode);
            #[cfg(not(unix))]
            let _ = mode;
            let file = options.open(tmp)?;
            match file
                .try_clone()
                .and_then(|lock| FileLock::lock(lock, LockMode::Exclusive))
            {
                Ok(lock) => {
                    held = Some(Held { _lock: lock });
                    Ok(file)
                }
                Err(error) => {
                    drop(file);
                    let _ = std::fs::remove_file(tmp);
                    Err(error)
                }
            }
        },
        super::atomic::Publish::ReplaceUnsynced,
    )?;
    held.ok_or_else(|| io::Error::other("published file was never opened"))
}

/// Read at most `max_bytes` of `path` while its writer holds it.
///
/// `None` when the file is missing or no writer holds it any more: its
/// contents are then nobody's, which is no better than absent.
///
/// # Errors
/// Returns an I/O error, an invalid bound, refusal when the file exceeds
/// the bound (its bytes are never returned), or an error when the file kept
/// being replaced while it was read.
pub fn read(path: &Path, max_bytes: impl TryInto<u64>) -> io::Result<Option<Vec<u8>>> {
    let max_bytes: u64 = max_bytes
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file byte bound is invalid"))?;
    for _ in 0..MAX_REPLACEMENTS_FOLLOWED {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let bytes = super::bounded::read_from(&file, max_bytes)?;
        match FileLock::try_lock(file, LockMode::Shared) {
            Err(TryLockError::WouldBlock) => return Ok(Some(bytes)),
            Err(TryLockError::Error(error)) => return Err(error),
            Ok(unheld) => {
                // The writer let this file go: it is gone, or it replaced
                // the file (and then let the old one go). Only the latter
                // leaves a newer file at `path` to read.
                if !replaced(unheld.file(), path)? {
                    return Ok(None);
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "file kept being replaced while it was read",
    ))
}

/// Whether `path` no longer names `file`.
fn replaced(file: &File, path: &Path) -> io::Result<bool> {
    let current = match std::fs::metadata(path) {
        Ok(current) => current,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let read = file.metadata()?;
        Ok((read.dev(), read.ino()) != (current.dev(), current.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = (file, current);
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_file_reads_until_its_writer_lets_go() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("current.json");
        assert_eq!(read(&path, 64).unwrap(), None, "nothing published yet");

        let first = publish(&path, b"first", 0o600).unwrap();
        assert_eq!(read(&path, 64).unwrap().as_deref(), Some(&b"first"[..]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // A replacement is read from the moment it is in place, even after
        // the old file's hold is gone.
        let second = publish(&path, b"second", 0o600).unwrap();
        drop(first);
        assert_eq!(read(&path, 64).unwrap().as_deref(), Some(&b"second"[..]));
        assert!(read(&path, 3).is_err(), "the bound still applies");

        // The writer lets go (it exited, or a replacement failed): the file
        // is still there, and is no one's.
        drop(second);
        assert!(path.exists());
        assert_eq!(read(&path, 64).unwrap(), None);
    }

    #[test]
    fn a_reader_that_opened_the_old_file_follows_the_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("current.json");
        let first = publish(&path, b"first", 0o600).unwrap();
        let old = File::open(&path).unwrap();
        let second = publish(&path, b"second", 0o600).unwrap();
        drop(first);
        // The reader's open file is the old one, let go; the name is not.
        let unheld = FileLock::try_lock(old, LockMode::Shared).unwrap();
        assert!(replaced(unheld.file(), &path).unwrap());
        drop(unheld);
        assert_eq!(read(&path, 64).unwrap().as_deref(), Some(&b"second"[..]));
        drop(second);
    }

    #[test]
    fn readers_do_not_vouch_for_each_other() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("current.json");
        drop(publish(&path, b"orphan", 0o600).unwrap());
        // Another reader holding a shared lock does not make the file held.
        let other = FileLock::try_lock(File::open(&path).unwrap(), LockMode::Shared).unwrap();
        assert_eq!(read(&path, 64).unwrap(), None);
        drop(other);
    }
}
