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
pub(super) use unix::{check_directory, check_private_file, create_directory};
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use windows::{check_directory, check_private_file, create_directory};

fn refusal(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(windows)]
pub(super) use windows::{require_private_protected, with_private_creation_descriptor};
