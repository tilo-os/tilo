//! Exclusive advisory locks on reopened ownership and journal files.

use std::fs::{File, TryLockError};
use std::ops::{Deref, DerefMut};

/// A file under a non-blocking exclusive lock that is released explicitly
/// when dropped.
///
/// The lock (flock on Linux) belongs to the open file description. A thread
/// that spawns a helper process hands the child a copy of the whole
/// descriptor table, which lives until the child's exec closes it, so merely
/// closing our descriptor would leave the lock held through that copy and a
/// reopen at that moment would report a busy owner that no longer exists.
/// Unlocking releases the description's lock however many descriptors still
/// refer to it.
pub(super) struct ExclusiveLock(File);

impl ExclusiveLock {
    /// Locks `file` without waiting; a live owner yields
    /// [`TryLockError::WouldBlock`].
    pub(super) fn try_new(file: File) -> Result<Self, TryLockError> {
        file.try_lock()?;
        Ok(Self(file))
    }
}

impl Deref for ExclusiveLock {
    type Target = File;

    fn deref(&self) -> &File {
        &self.0
    }
}

impl DerefMut for ExclusiveLock {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.0
    }
}

impl Drop for ExclusiveLock {
    fn drop(&mut self) {
        // A failed unlock leaves only the close, which is the best left to do.
        let _ = self.0.unlock();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    #[test]
    fn a_live_owner_keeps_the_lock() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lock");
        let _owner = ExclusiveLock::try_new(File::create(&path).unwrap()).unwrap();
        assert!(matches!(
            ExclusiveLock::try_new(File::open(&path).unwrap()),
            Err(TryLockError::WouldBlock)
        ));
    }

    #[test]
    fn a_dropped_lock_is_free_while_a_spawning_child_shares_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("lock");
        let owner = ExclusiveLock::try_new(File::create(&path).unwrap()).unwrap();
        let (mut forked, forked_signal) = std::io::pipe().unwrap();
        // A sibling thread spawns a child that lingers before executing, so it
        // holds a copy of the lock descriptor while the owner is dropped here.
        let spawner = std::thread::spawn(move || {
            let signal = forked_signal.as_raw_fd();
            let mut command = std::process::Command::new("true");
            // SAFETY: the hook only calls async-signal-safe `write` and `nanosleep`.
            unsafe {
                command.pre_exec(move || {
                    libc::write(signal, b"f".as_ptr().cast(), 1);
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    Ok(())
                });
            }
            let status = command.status();
            drop(forked_signal);
            status.unwrap()
        });
        let mut byte = [0u8];
        assert_eq!(forked.read(&mut byte).unwrap(), 1, "child must fork");
        drop(owner);
        ExclusiveLock::try_new(File::open(&path).unwrap()).unwrap();
        assert!(spawner.join().unwrap().success());
    }
}
