//! Owned native file leases for coordinating processes through a sentinel file. A lease
//! normally uses a dedicated lock file that is never replaced: a publication renames a
//! new file into place, and a lease on the old one would coordinate nothing.

use advisory_lock::{FileLock, LockMode, TryLockError};
use std::error::Error;
use std::fmt;
use std::fs::{File, Metadata};
use std::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseMode {
    Shared,
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
    pub fn acquire(file: File, mode: LeaseMode) -> io::Result<Self> {
        FileLock::lock(file, mode.into()).map(|lock| Self { lock })
    }

    /// Without blocking; [`AcquireError::Busy`] when another process or handle holds a conflicting
    /// lease.
    pub fn try_acquire(file: File, mode: LeaseMode) -> Result<Self, AcquireError> {
        match FileLock::try_lock(file, mode.into()) {
            Ok(lock) => Ok(Self { lock }),
            Err(TryLockError::WouldBlock) => Err(AcquireError::Busy),
            Err(TryLockError::Error(error)) => Err(AcquireError::Io(error)),
        }
    }

    /// Queries native identity through the handle that owns the lease.
    #[cfg(any(unix, windows))]
    pub fn identity(&self) -> io::Result<crate::native::atomic::FileIdentity> {
        crate::native::atomic::FileIdentity::of(self.lock.file())
    }

    pub fn metadata(&self) -> io::Result<Metadata> {
        self.lock.file().metadata()
    }
}
