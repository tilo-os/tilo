//! Extended file attributes.
//!
//! The Tilo filesystem projection exposes metadata and accepts control
//! commands as extended attributes. Where the host has no extended attributes
//! every operation fails with [`io::ErrorKind::Unsupported`] and changes
//! nothing, so a control command is never acknowledged by storing an
//! unrelated local attribute.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::Path;

/// Whether this host has extended attributes.
pub const SUPPORTED: bool = cfg!(unix);

/// Read attribute `name` of `path`; `Ok(None)` when it is not set.
///
/// # Errors
/// Returns the host's error, or `Unsupported` without extended attributes.
#[inline]
pub fn get(path: impl AsRef<Path>, name: impl AsRef<OsStr>) -> io::Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    {
        xattr::get(path, name)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, name);
        Err(unavailable())
    }
}

/// Set attribute `name` of `path` to `value`.
///
/// # Errors
/// Returns the host's error, or `Unsupported` without extended attributes.
#[inline]
pub fn set(path: impl AsRef<Path>, name: impl AsRef<OsStr>, value: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        xattr::set(path, name, value)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, name, value);
        Err(unavailable())
    }
}

/// Remove attribute `name` of `path`.
///
/// # Errors
/// Returns the host's error, or `Unsupported` without extended attributes.
#[inline]
pub fn remove(path: impl AsRef<Path>, name: impl AsRef<OsStr>) -> io::Result<()> {
    #[cfg(unix)]
    {
        xattr::remove(path, name)
    }
    #[cfg(not(unix))]
    {
        let _ = (path, name);
        Err(unavailable())
    }
}

/// Read attribute `name` of an open file; `Ok(None)` when it is not set.
///
/// # Errors
/// Returns the host's error, or `Unsupported` without extended attributes.
#[inline]
pub fn get_open(file: &File, name: impl AsRef<OsStr>) -> io::Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    {
        xattr::FileExt::get_xattr(file, name)
    }
    #[cfg(not(unix))]
    {
        let _ = (file, name);
        Err(unavailable())
    }
}

/// Set attribute `name` of an open file to `value`.
///
/// # Errors
/// Returns the host's error, or `Unsupported` without extended attributes.
#[inline]
pub fn set_open(file: &File, name: impl AsRef<OsStr>, value: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        xattr::FileExt::set_xattr(file, name, value)
    }
    #[cfg(not(unix))]
    {
        let _ = (file, name, value);
        Err(unavailable())
    }
}

#[cfg(not(unix))]
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "extended attributes are unavailable on this host",
    )
}

/// The host error codes an attribute-based protocol (such as the Tilo
/// filesystem projection's control attributes) answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    /// The attribute is not set (`ENODATA`).
    NoData,
    /// The request is already in effect (`EALREADY`).
    Already,
    /// The object is busy (`EBUSY`).
    Busy,
    /// Try again later (`EAGAIN`).
    Again,
    /// No such file (`ENOENT`).
    NotFound,
    /// Read-only filesystem (`EROFS`).
    ReadOnlyFilesystem,
    /// Access denied (`EACCES`).
    AccessDenied,
    /// Operation not permitted (`EPERM`).
    NotPermitted,
    /// Operation not supported (`ENOTSUP`).
    NotSupported,
    /// Invalid argument (`EINVAL`).
    InvalidArgument,
    /// Value too large (`E2BIG`).
    TooBig,
    /// The request started work that is still running (`EINPROGRESS`).
    InProgress,
}

impl ErrorCode {
    /// The code `error` carries, if it is one of these.
    #[must_use]
    pub fn of(error: &io::Error) -> Option<Self> {
        #[cfg(unix)]
        {
            Some(match error.raw_os_error()? {
                libc::ENODATA => Self::NoData,
                libc::EALREADY => Self::Already,
                libc::EBUSY => Self::Busy,
                libc::EAGAIN => Self::Again,
                libc::ENOENT => Self::NotFound,
                libc::EROFS => Self::ReadOnlyFilesystem,
                libc::EACCES => Self::AccessDenied,
                libc::EPERM => Self::NotPermitted,
                libc::ENOTSUP => Self::NotSupported,
                libc::EINVAL => Self::InvalidArgument,
                libc::E2BIG => Self::TooBig,
                libc::EINPROGRESS => Self::InProgress,
                _ => return None,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = error;
            None
        }
    }

    /// An I/O error carrying this code.
    #[must_use]
    pub fn into_io_error(self) -> io::Error {
        #[cfg(unix)]
        {
            io::Error::from_raw_os_error(match self {
                Self::NoData => libc::ENODATA,
                Self::Already => libc::EALREADY,
                Self::Busy => libc::EBUSY,
                Self::Again => libc::EAGAIN,
                Self::NotFound => libc::ENOENT,
                Self::ReadOnlyFilesystem => libc::EROFS,
                Self::AccessDenied => libc::EACCES,
                Self::NotPermitted => libc::EPERM,
                Self::NotSupported => libc::ENOTSUP,
                Self::InvalidArgument => libc::EINVAL,
                Self::TooBig => libc::E2BIG,
                Self::InProgress => libc::EINPROGRESS,
            })
        }
        #[cfg(not(unix))]
        {
            io::Error::other(format!("{self:?}"))
        }
    }
}
