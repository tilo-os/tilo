//! Owned native file leases for coordinating processes through a sentinel file.
//!
//! A lease should normally use a dedicated sentinel whose identity does not
//! change while processes coordinate. Native file-lock behavior varies by
//! platform and network filesystem; callers must validate filesystems whose
//! lock semantics matter to them.
//!
//! [`FileLease`] consumes its open [`File`]. Callers must not retain duplicated
//! handles to that same open file, because closing or unlocking a duplicate can
//! affect the native lease on some platforms. This API promises locking only:
//! it does not provide private file creation, attest the sentinel's identity, or
//! durably publish its containing directory.

use advisory_lock::{FileLock, LockMode, TryLockError};
use std::error::Error;
use std::fmt;
use std::fs::{File, Metadata};
use std::io;

/// The kind of native lease to acquire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseMode {
    /// Coexists with other shared leases and excludes exclusive leases.
    Shared,
    /// Excludes both shared and exclusive leases.
    Exclusive,
}

impl From<LeaseMode> for LockMode {
    fn from(mode: LeaseMode) -> Self {
        match mode {
            LeaseMode::Shared => Self::Shared,
            LeaseMode::Exclusive => Self::Exclusive,
        }
    }
}

/// A failure to acquire a lease without blocking.
#[derive(Debug)]
pub enum AcquireError {
    /// Another process or handle holds a conflicting lease.
    Busy,
    /// The operating system could not attempt the lock operation.
    Io(io::Error),
}

impl fmt::Display for AcquireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("file lease is busy"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl Error for AcquireError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Busy => None,
            Self::Io(error) => Some(error),
        }
    }
}

/// An owned native lease released when its private file handle is dropped.
#[derive(Debug)]
pub struct FileLease {
    lock: FileLock,
}

impl FileLease {
    /// Block until `mode` can be acquired on `file`.
    ///
    /// # Errors
    ///
    /// Returns the native I/O error when the operating system cannot acquire
    /// the requested lease.
    pub fn acquire(file: File, mode: LeaseMode) -> io::Result<Self> {
        FileLock::lock(file, mode.into()).map(|lock| Self { lock })
    }

    /// Attempt to acquire `mode` on `file` without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`AcquireError::Busy`] for a conflicting lease and preserves
    /// any native locking failure as [`AcquireError::Io`].
    pub fn try_acquire(file: File, mode: LeaseMode) -> Result<Self, AcquireError> {
        match FileLock::try_lock(file, mode.into()) {
            Ok(lock) => Ok(Self { lock }),
            Err(TryLockError::WouldBlock) => Err(AcquireError::Busy),
            Err(TryLockError::Error(error)) => Err(AcquireError::Io(error)),
        }
    }

    /// Query native identity through the handle that owns the lease.
    ///
    /// # Errors
    /// Returns the native metadata error when identity cannot be observed.
    #[cfg(any(unix, windows))]
    pub fn identity(&self) -> io::Result<crate::native::atomic::FileIdentity> {
        crate::native::atomic::FileIdentity::of(self.lock.file())
    }

    /// Read metadata through the handle that owns the lease.
    ///
    /// # Errors
    ///
    /// Returns the native I/O error when metadata cannot be read.
    pub fn metadata(&self) -> io::Result<Metadata> {
        self.lock.file().metadata()
    }
}
