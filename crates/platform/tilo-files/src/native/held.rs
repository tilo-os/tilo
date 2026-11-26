//! A file its writer stands behind for as long as it lives: published with the writer's
//! lock on it, and read as current only while that lock is held. The writer keeps the
//! [`Held`] it got from [`publish`]. When it exits, crashes or lets go, the kernel
//! releases the lock with it, so a reader learns that the contents are no longer
//! vouched for at once: nothing is renewed on a clock. The lock is an exclusive
//! `flock(2)` taken before the file is renamed into place, so the name never names a
//! live writer's file before it is locked. A reader tries a shared lock: refused means
//! the writer holds it, granted means nobody does unless the name was replaced
//! meanwhile by a newer file, which the reader then reads. Readers never conflict with
//! one another and can neither take the writer's place nor block it.

use std::fs::File;
use std::io;
use std::path::Path;

use super::lock::{FileLock, LockMode, TryLockError};

/// A writer replaces the file only when its contents change, so following replacements ends.
const MAX_REPLACEMENTS_FOLLOWED: usize = 8;

/// Dropping it withdraws the file's contents from readers.
#[derive(Debug)]
#[must_use = "the published file is current only while this is held"]
pub struct Held {
    _lock: FileLock,
}

/// Like [`super::atomic::write_regenerable_with_mode`], the file and its directory are not synced: this
/// is for a runtime directory, which a crash discards along with the writer.
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
            let locked = file
                .try_clone()
                .and_then(|lock| FileLock::lock(lock, LockMode::Exclusive))
                // On failure the file closes with this unused closure, before the temporary is
                // removed.
                .map(move |lock| (file, lock));
            match locked {
                Ok((file, lock)) => {
                    held = Some(Held { _lock: lock });
                    Ok(file)
                }
                Err(error) => {
                    let _ = std::fs::remove_file(tmp);
                    Err(error)
                }
            }
        },
        super::atomic::Publish::ReplaceUnsynced,
    )?;
    held.ok_or_else(|| io::Error::other("published file was never opened"))
}

/// `None` when the file is missing or no writer holds it any more: its contents are then nobody's,
/// which is no better than absent. Refused when the file exceeds the bound, whose bytes are never
/// returned.
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
                // The writer let this file go: it is gone, or it replaced the file and then let the
                // old one go. Only the latter leaves a newer file at `path` to read.
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
