//! The one error type of every [`Dir`](crate::Dir) operation.

use std::fmt;
use std::io;

use crate::caps::{Feature, Unsupported};

/// Why a file operation failed. Every backend reports the same variants for
/// the same situation, so callers match on them rather than on OS codes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FsError {
    NotFound,
    AlreadyExists,
    /// A path component that must be a directory is a file.
    NotADirectory,
    /// The operation needs a file and the path names a directory.
    IsADirectory,
    DirectoryNotEmpty,
    PermissionDenied,
    /// The path cannot be used for this operation, such as replacing the
    /// root of a [`Dir`](crate::Dir) with a file.
    InvalidPath(&'static str),
    /// This target, or this kind of [`Dir`](crate::Dir), cannot do it.
    Unsupported {
        feature: Feature,
        reason: &'static str,
    },
    /// Anything else, with the platform's description.
    Io(String),
}

impl fmt::Display for FsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such file or directory"),
            Self::AlreadyExists => f.write_str("the name is already taken"),
            Self::NotADirectory => f.write_str("a path component is not a directory"),
            Self::IsADirectory => f.write_str("the path is a directory"),
            Self::DirectoryNotEmpty => f.write_str("the directory is not empty"),
            Self::PermissionDenied => f.write_str("permission denied"),
            Self::InvalidPath(reason) => write!(f, "invalid path: {reason}"),
            Self::Unsupported { reason, .. } => f.write_str(reason),
            Self::Io(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for FsError {}

impl From<Unsupported> for FsError {
    fn from(unsupported: Unsupported) -> Self {
        Self::Unsupported {
            feature: unsupported.feature,
            reason: unsupported.reason,
        }
    }
}

impl From<io::Error> for FsError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            io::ErrorKind::AlreadyExists => Self::AlreadyExists,
            io::ErrorKind::NotADirectory => Self::NotADirectory,
            io::ErrorKind::IsADirectory => Self::IsADirectory,
            io::ErrorKind::DirectoryNotEmpty => Self::DirectoryNotEmpty,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            _ => Self::Io(error.to_string()),
        }
    }
}
