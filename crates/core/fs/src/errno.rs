//! The filesystem answers in POSIX error numbers, FUSE's own language, on every host. On Unix an
//! error number is the system's own error; Windows' system errors are other numbers, so there an
//! error number travels inside the error it makes, and a system error is read back by its kind.

use std::io;

/// A quota was exceeded.
#[cfg(unix)]
pub use libc::EDQUOT;
/// A quota was exceeded. Windows' C library has no number for it; this is Linux's, which no other
/// error here takes.
#[cfg(not(unix))]
pub const EDQUOT: i32 = 122;

/// `errno` as an error.
#[must_use]
pub fn error(errno: i32) -> io::Error {
    #[cfg(unix)]
    {
        io::Error::from_raw_os_error(errno)
    }
    #[cfg(not(unix))]
    {
        io::Error::new(kind(errno), Errno(errno))
    }
}

/// The error number `error` is, `EIO` where it names none.
#[must_use]
pub fn code(error: &io::Error) -> i32 {
    #[cfg(unix)]
    {
        error.raw_os_error().unwrap_or(libc::EIO)
    }
    #[cfg(not(unix))]
    {
        if let Some(Errno(errno)) = error.get_ref().and_then(|inner| inner.downcast_ref()) {
            return *errno;
        }
        KINDS
            .iter()
            .find(|(_, kind)| *kind == error.kind())
            .map_or(libc::EIO, |(errno, _)| *errno)
    }
}

/// Each error number a host error is read back as, by its kind.
#[cfg(not(unix))]
const KINDS: [(i32, io::ErrorKind); 15] = [
    (libc::ENOENT, io::ErrorKind::NotFound),
    (libc::EACCES, io::ErrorKind::PermissionDenied),
    (libc::EEXIST, io::ErrorKind::AlreadyExists),
    (libc::EINVAL, io::ErrorKind::InvalidInput),
    (libc::ENOSPC, io::ErrorKind::StorageFull),
    (EDQUOT, io::ErrorKind::QuotaExceeded),
    (libc::EFBIG, io::ErrorKind::FileTooLarge),
    (libc::ENOTDIR, io::ErrorKind::NotADirectory),
    (libc::EISDIR, io::ErrorKind::IsADirectory),
    (libc::ENOTEMPTY, io::ErrorKind::DirectoryNotEmpty),
    (libc::EROFS, io::ErrorKind::ReadOnlyFilesystem),
    (libc::EBUSY, io::ErrorKind::ResourceBusy),
    (libc::EXDEV, io::ErrorKind::CrossesDevices),
    (libc::ENOSYS, io::ErrorKind::Unsupported),
    (libc::EINTR, io::ErrorKind::Interrupted),
];

#[cfg(not(unix))]
fn kind(errno: i32) -> io::ErrorKind {
    KINDS
        .iter()
        .find(|(number, _)| *number == errno)
        .map_or(io::ErrorKind::Other, |(_, kind)| *kind)
}

/// An error number, carried in the error it makes.
#[cfg(not(unix))]
#[derive(Debug)]
struct Errno(i32);

#[cfg(not(unix))]
impl std::fmt::Display for Errno {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (error {})", kind(self.0), self.0)
    }
}

#[cfg(not(unix))]
impl std::error::Error for Errno {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_number_survives_the_error_it_makes() {
        for errno in [libc::ENOENT, libc::EEXIST, libc::EIO, EDQUOT, libc::EXDEV] {
            assert_eq!(code(&error(errno)), errno);
        }
        assert_eq!(error(libc::ENOENT).kind(), io::ErrorKind::NotFound);
        assert_eq!(code(&io::Error::other("no number")), libc::EIO);
    }
}
