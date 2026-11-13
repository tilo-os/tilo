//! The one error type of every [`Dir`](crate::Dir) operation.

use std::fmt;
use std::io;

use crate::caps::{Feature, Unsupported};

/// Why a file operation failed. Every backend reports the same variants for the same situation, so
/// callers match on them across targets.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FsError {
    NotFound,
    AlreadyExists,
    NotADirectory,
    IsADirectory,
    DirectoryNotEmpty,
    PermissionDenied,
    InvalidPath(&'static str),
    Unsupported {
        feature: Feature,
        reason: &'static str,
    },
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
