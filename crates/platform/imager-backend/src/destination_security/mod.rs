//! Destination namespace protection, separate from read-only source access.
//!
//! The guarantee excludes hostile processes running as the current account or
//! an administrator: those principals can alter final output regardless of the
//! copy protocol. Other accounts must not be able to replace operation-owned
//! names between identity checks and mkdir/link/unlink. Unknown security or
//! filesystem semantics are a refusal, not an assumption of private access.

use std::io;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{check_directory, check_private_file, create_directory};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{check_directory, check_private_file, create_directory};

#[cfg(all(windows, feature = "test-support"))]
pub use windows::create_acl_fixture;

/// # Errors
/// Always refuses platforms without a supported namespace-security adapter.
#[cfg(not(any(unix, windows)))]
pub fn check_directory(
    _file: &std::fs::File,
    _child: Option<&cap_primitives::fs::Metadata>,
    _private: bool,
) -> io::Result<()> {
    Err(refusal(
        "destination namespace protection is unavailable on this platform",
    ))
}

/// # Errors
/// Always refuses platforms without a supported namespace-security adapter.
#[cfg(not(any(unix, windows)))]
pub fn check_private_file(_file: &std::fs::File) -> io::Result<()> {
    Err(refusal(
        "destination namespace protection is unavailable on this platform",
    ))
}

/// # Errors
/// Always refuses platforms without a supported namespace-security adapter.
#[cfg(not(any(unix, windows)))]
pub fn create_directory(_parent: &std::fs::File, _name: &std::ffi::OsStr) -> io::Result<()> {
    Err(refusal(
        "destination namespace protection is unavailable on this platform",
    ))
}

fn refusal(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
