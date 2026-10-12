//! Advisory whole-file locks with one guard type, [`FileLock`].
//!
//! # What the lock belongs to
//!
//! On Unix the lock is `flock(2)`; elsewhere it is the standard library's
//! file lock (`LockFileEx` on Windows). A `flock` lock belongs to the *open
//! file description*, not to a descriptor, a thread or a process:
//!
//! - Every descriptor that refers to the same description shares the lock.
//!   `File::try_clone`, `dup(2)` and `fork(2)` all create such descriptors.
//! - Two independent `open(2)` calls create two descriptions, and their locks
//!   conflict even inside one process and even between threads. Opening the
//!   file again is therefore the way to test contention in-process.
//! - The kernel drops the lock when the last descriptor of the description is
//!   closed, or at once when any of them is explicitly unlocked.
//!
//! # Why drop unlocks explicitly
//!
//! A thread that spawns a child hands the child a copy of the whole
//! descriptor table. The copy lives until the child's `exec` closes it (the
//! lock fd is always `FD_CLOEXEC`, see below), or for the child's whole life
//! if it never execs. Merely closing our descriptor during that window leaves
//! the description, and so the lock, alive in the child: a peer would then see
//! a busy owner that has already gone. [`FileLock`] therefore calls
//! `flock(LOCK_UN)` before closing, which releases the lock however many
//! inherited descriptors still refer to the description.
//!
//! The same rule means a forked child must not drop a `FileLock` it inherited
//! from its parent: the unlock would release the parent's lock too. Children
//! created with `std::process::Command` never run Rust destructors between
//! fork and exec, so this only concerns code that calls `fork(2)` directly.
//!
//! # Close-on-exec
//!
//! Every guard sets `FD_CLOEXEC` on its descriptor when it is constructed, so
//! a lock is never handed to an executed program, whatever flags the caller
//! opened the file with.
//!
//! # No waiting policy
//!
//! Blocking variants wait in the kernel until the lock is granted; `try`
//! variants return [`TryLockError::WouldBlock`] at once. There is deliberately
//! no retry or timeout: a caller that cannot wait uses a `try` variant and
//! reports the busy owner.

// The browser has no files: there `std::fs::File` is an uninhabited stub, no
// `FileLock` can be built, and its accessors are never reached.
#![cfg_attr(
    target_arch = "wasm32",
    allow(
        clippy::uninhabited_references,
        reason = "std::fs::File is uninhabited on wasm32-unknown-unknown; no guard exists there"
    )
)]

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::{Deref, DerefMut};
use std::path::Path;

pub use std::fs::TryLockError;

/// Whether other holders may lock the file at the same time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMode {
    /// Any number of shared holders, and no exclusive holder.
    Shared,
    /// Exactly one holder.
    Exclusive,
}

/// A file held under an advisory lock until the guard is dropped.
///
/// Dropping the guard unlocks the file explicitly and then closes it; see the
/// [crate documentation](crate) for why closing alone is not enough. The
/// guard dereferences to the locked [`File`], so callers that keep state in
/// the lock file read and write it through the guard.
#[derive(Debug)]
#[must_use = "the lock is released when the guard is dropped"]
pub struct FileLock {
    file: File,
    mode: LockMode,
}

impl FileLock {
    /// Waits until `file` is locked in `mode`.
    ///
    /// # Errors
    /// Returns the operating system's error when the file cannot be locked
    /// or its descriptor cannot be marked close-on-exec.
    pub fn lock(file: File, mode: LockMode) -> io::Result<Self> {
        sys::set_cloexec(&file)?;
        sys::lock(&file, mode)?;
        Ok(Self { file, mode })
    }

    /// Locks `file` in `mode` without waiting.
    ///
    /// # Errors
    /// [`TryLockError::WouldBlock`] while a conflicting lock is held, and
    /// [`TryLockError::Error`] for any other failure.
    pub fn try_lock(file: File, mode: LockMode) -> Result<Self, TryLockError> {
        sys::set_cloexec(&file).map_err(TryLockError::Error)?;
        sys::try_lock(&file, mode)?;
        Ok(Self { file, mode })
    }

    /// Waits for an exclusive lock on `file`.
    ///
    /// # Errors
    /// See [`FileLock::lock`].
    pub fn exclusive(file: File) -> io::Result<Self> {
        Self::lock(file, LockMode::Exclusive)
    }

    /// Waits for a shared lock on `file`.
    ///
    /// # Errors
    /// See [`FileLock::lock`].
    pub fn shared(file: File) -> io::Result<Self> {
        Self::lock(file, LockMode::Shared)
    }

    /// Takes an exclusive lock on `file` without waiting.
    ///
    /// # Errors
    /// See [`FileLock::try_lock`].
    pub fn try_exclusive(file: File) -> Result<Self, TryLockError> {
        Self::try_lock(file, LockMode::Exclusive)
    }

    /// Takes a shared lock on `file` without waiting.
    ///
    /// # Errors
    /// See [`FileLock::try_lock`].
    pub fn try_shared(file: File) -> Result<Self, TryLockError> {
        Self::try_lock(file, LockMode::Shared)
    }

    /// Opens (creating if missing, never truncating) the lock file at `path`
    /// for reading and writing, then waits for a lock in `mode`.
    ///
    /// Callers with ownership or permission requirements open the file
    /// themselves and pass it to [`FileLock::lock`].
    ///
    /// # Errors
    /// Returns the error from opening or locking the file.
    pub fn open(path: &Path, mode: LockMode) -> io::Result<Self> {
        Self::lock(open_lock_file(path)?, mode)
    }

    /// Like [`FileLock::open`], but never waits.
    ///
    /// # Errors
    /// [`TryLockError::WouldBlock`] while a conflicting lock is held, and
    /// [`TryLockError::Error`] when opening or locking fails otherwise.
    pub fn try_open(path: &Path, mode: LockMode) -> Result<Self, TryLockError> {
        Self::try_lock(open_lock_file(path).map_err(TryLockError::Error)?, mode)
    }

    /// Like [`FileLock::open`], but the lock is on the file `path` names when
    /// it is granted.
    ///
    /// A lock file that names a short-lived thing (a lease on one directory)
    /// is removed with that thing, by an exclusive holder, so it does not
    /// outlive it. A waiter that opened the old file would then hold a lock
    /// nobody else can ever contend for. So once the lock is granted, the
    /// locked file is compared with the one at `path`, and on a mismatch the
    /// lock is dropped and taken again on the current file. Holders that
    /// remove the file must do so only while holding it exclusively.
    ///
    /// # Errors
    /// Returns the error from opening or locking the file.
    pub fn open_current(path: &Path, mode: LockMode) -> io::Result<Self> {
        loop {
            let lock = Self::open(path, mode)?;
            if sys::names(path, &lock.file)? {
                return Ok(lock);
            }
        }
    }

    /// Like [`FileLock::open_current`], but never waits.
    ///
    /// # Errors
    /// [`TryLockError::WouldBlock`] while a conflicting lock is held, and
    /// [`TryLockError::Error`] when opening or locking fails otherwise.
    pub fn try_open_current(path: &Path, mode: LockMode) -> Result<Self, TryLockError> {
        loop {
            let lock = Self::try_open(path, mode)?;
            if sys::names(path, &lock.file).map_err(TryLockError::Error)? {
                return Ok(lock);
            }
        }
    }

    /// The mode this guard holds.
    pub fn mode(&self) -> LockMode {
        self.mode
    }

    /// The locked file.
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Transfers the file without explicitly unlocking its open description.
    ///
    /// This is for an executed child validating a lock inherited from its
    /// parent: dropping a normal guard would unlock the parent's transaction.
    /// The returned file closes normally; the lock survives while any parent
    /// descriptor still refers to that same open description. Ordinary lock
    /// owners should keep the guard so its explicit unlock-on-drop remains.
    #[cfg(unix)]
    pub fn into_file_preserving_lock(self) -> File {
        let guard = std::mem::ManuallyDrop::new(self);
        // SAFETY: suppressing Drop leaves the file initialized; read transfers
        // its unique ownership exactly once, without executing the unlock.
        unsafe { std::ptr::read(&raw const guard.file) }
    }

    /// Waits until the lock is converted to `mode`.
    ///
    /// A conversion is not atomic: the kernel may release the old lock before
    /// granting the new one, so another holder can run in between and
    /// anything validated under the old lock must be validated again.
    ///
    /// # Errors
    /// Returns the operating system's error. The guard is dropped, and so
    /// unlocked, when conversion fails.
    pub fn convert(mut self, mode: LockMode) -> io::Result<Self> {
        if self.mode != mode {
            sys::convert(&self.file, mode)?;
            self.mode = mode;
        }
        Ok(self)
    }

    /// Converts the lock to `mode` without waiting.
    ///
    /// The same non-atomicity as [`FileLock::convert`] applies.
    ///
    /// # Errors
    /// [`TryLockError::WouldBlock`] while a conflicting lock is held. The guard
    /// is dropped, and so unlocked, when conversion fails.
    pub fn try_convert(mut self, mode: LockMode) -> Result<Self, TryLockError> {
        if self.mode != mode {
            sys::try_convert(&self.file, mode)?;
            self.mode = mode;
        }
        Ok(self)
    }

    /// Replaces the lock file's contents with this process's id, so a peer
    /// that finds the lock busy can say who holds it ([`owner_pid`]).
    ///
    /// The record is diagnostic only. It can be stale or momentarily empty,
    /// so never use it to decide whether the lock is free.
    ///
    /// # Errors
    /// Rejects a shared guard, whose file other holders may be using, and
    /// returns write errors.
    pub fn record_owner(&self) -> io::Result<()> {
        if self.mode != LockMode::Exclusive {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only an exclusive lock holder may record itself as owner",
            ));
        }
        let mut file = &self.file;
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(std::process::id().to_string().as_bytes())?;
        file.flush()
    }

    /// Unlocks and closes the file, reporting an unlock failure that
    /// dropping the guard would ignore.
    ///
    /// # Errors
    /// Returns the operating system's unlock error.
    pub fn unlock(self) -> io::Result<()> {
        // The destructor's second unlock of an unlocked file is a no-op.
        sys::unlock(&self.file)
    }
}

impl Deref for FileLock {
    type Target = File;

    fn deref(&self) -> &File {
        &self.file
    }
}

impl DerefMut for FileLock {
    fn deref_mut(&mut self) -> &mut File {
        &mut self.file
    }
}

impl AsRef<File> for FileLock {
    fn as_ref(&self) -> &File {
        &self.file
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // A failed unlock leaves only the close, which is the best left to do.
        let _ = sys::unlock(&self.file);
    }
}

/// Opens `path` for reading and writing, creating it if it is missing and
/// never truncating an existing file.
///
/// # Errors
/// Returns the error from opening the file.
pub fn open_lock_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// The process id a holder recorded with [`FileLock::record_owner`], if the
/// lock file holds one. Diagnostic only: the process may have exited.
pub fn owner_pid(path: &Path) -> Option<u32> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(32)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse().ok().filter(|pid| *pid != 0)
}

#[cfg(unix)]
#[allow(
    clippy::disallowed_methods,
    reason = "this is the one wrapper the workspace routes file locks through"
)]
mod sys {
    use super::{LockMode, TryLockError};
    use rustix::fs::FlockOperation;
    use rustix::io::{Errno, FdFlags};
    use std::fs::File;
    use std::io;

    pub(super) fn set_cloexec(file: &File) -> io::Result<()> {
        let flags = rustix::io::fcntl_getfd(file)?;
        if !flags.contains(FdFlags::CLOEXEC) {
            rustix::io::fcntl_setfd(file, flags | FdFlags::CLOEXEC)?;
        }
        Ok(())
    }

    fn blocking(mode: LockMode) -> FlockOperation {
        match mode {
            LockMode::Shared => FlockOperation::LockShared,
            LockMode::Exclusive => FlockOperation::LockExclusive,
        }
    }

    fn non_blocking(mode: LockMode) -> FlockOperation {
        match mode {
            LockMode::Shared => FlockOperation::NonBlockingLockShared,
            LockMode::Exclusive => FlockOperation::NonBlockingLockExclusive,
        }
    }

    /// `flock` restarts neither a wait nor an unlock interrupted by a signal
    /// handler installed without `SA_RESTART`; resuming the same call is the
    /// documented response and says nothing about contention.
    fn flock(file: &File, operation: FlockOperation) -> Result<(), Errno> {
        loop {
            match rustix::fs::flock(file, operation) {
                Err(Errno::INTR) => {}
                result => return result,
            }
        }
    }

    fn try_flock(file: &File, operation: FlockOperation) -> Result<(), TryLockError> {
        flock(file, operation).map_err(|errno| {
            if errno == Errno::WOULDBLOCK {
                TryLockError::WouldBlock
            } else {
                TryLockError::Error(errno.into())
            }
        })
    }

    pub(super) fn lock(file: &File, mode: LockMode) -> io::Result<()> {
        Ok(flock(file, blocking(mode))?)
    }

    pub(super) fn try_lock(file: &File, mode: LockMode) -> Result<(), TryLockError> {
        try_flock(file, non_blocking(mode))
    }

    // `flock` converts a held lock in place when called with another mode.
    pub(super) fn convert(file: &File, mode: LockMode) -> io::Result<()> {
        lock(file, mode)
    }

    pub(super) fn try_convert(file: &File, mode: LockMode) -> Result<(), TryLockError> {
        try_lock(file, mode)
    }

    pub(super) fn unlock(file: &File) -> io::Result<()> {
        Ok(flock(file, FlockOperation::Unlock)?)
    }

    /// Whether `path` names `file` now: same device and inode. A missing
    /// `path` names nothing.
    pub(super) fn names(path: &std::path::Path, file: &File) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt;
        let held = file.metadata()?;
        match std::fs::metadata(path) {
            Ok(named) => Ok(named.dev() == held.dev() && named.ino() == held.ino()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[cfg(not(unix))]
#[allow(
    clippy::disallowed_methods,
    reason = "this is the one wrapper the workspace routes file locks through"
)]
mod sys {
    use super::{LockMode, TryLockError};
    use std::fs::File;
    use std::io;

    // Handles are not inherited by spawned processes unless a caller
    // explicitly marks them inheritable, which the standard library never
    // does for files it opens.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "shares the fallible signature of the Unix implementation"
    )]
    pub(super) fn set_cloexec(_file: &File) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn lock(file: &File, mode: LockMode) -> io::Result<()> {
        match mode {
            LockMode::Shared => file.lock_shared(),
            LockMode::Exclusive => file.lock(),
        }
    }

    pub(super) fn try_lock(file: &File, mode: LockMode) -> Result<(), TryLockError> {
        match mode {
            LockMode::Shared => file.try_lock_shared(),
            LockMode::Exclusive => file.try_lock(),
        }
    }

    // Windows byte-range locks stack rather than convert, so release first.
    pub(super) fn convert(file: &File, mode: LockMode) -> io::Result<()> {
        file.unlock()?;
        lock(file, mode)
    }

    pub(super) fn try_convert(file: &File, mode: LockMode) -> Result<(), TryLockError> {
        file.unlock().map_err(TryLockError::Error)?;
        try_lock(file, mode)
    }

    pub(super) fn unlock(file: &File) -> io::Result<()> {
        file.unlock()
    }

    // A file the standard library opened cannot be removed while it is
    // open, so the name still refers to it.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "shares the fallible signature of the Unix implementation"
    )]
    pub(super) fn names(_path: &std::path::Path, _file: &File) -> io::Result<bool> {
        Ok(true)
    }
}
