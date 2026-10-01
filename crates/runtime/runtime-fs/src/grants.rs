//! Opaque, live filesystem authority. Paths and names never confer a grant.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

use crate::native::GrantLease;

/// Authenticated workspace/application identity supplied by the trusted host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FsOwner {
    pub workspace: String,
    pub app_id: String,
}

/// Rights recorded at admission. Mutation rights are reserved for operation providers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsRight {
    Enumerate,
    Read,
    Create,
    Rename,
    Copy,
    Move,
    Trash,
    Purge,
}

/// A reference to an owned directory descriptor and its live ancestor lineage.
#[derive(Clone, Debug)]
pub struct DirectoryGrant {
    pub(super) lease: GrantLease,
}

/// Bytes/metadata authority; an isolated file never grants access to its parent.
#[derive(Clone, Debug)]
pub struct FileGrant {
    pub(super) lease: GrantLease,
}

#[derive(Clone, Debug)]
pub enum GrantedEntry {
    Directory(DirectoryGrant),
    File(FileGrant),
}

impl GrantedEntry {
    pub(super) fn lease(&self) -> &GrantLease {
        match self {
            Self::Directory(grant) => &grant.lease,
            Self::File(grant) => &grant.lease,
        }
    }
}

/// Exactly one native component. Lossy display labels must not be used as identity.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct EntryName(OsString);

impl EntryName {
    /// # Errors
    /// Returns `InvalidName` for empty, dot, parent, slash or NUL components.
    pub fn new(value: OsString) -> Result<Self, FsError> {
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.contains(&b'/')
            || bytes.contains(&0)
        {
            return Err(FsError::InvalidName);
        }
        Ok(Self(value))
    }

    pub fn as_os_str(&self) -> &OsStr {
        &self.0
    }
}

/// Native filesystem failures retain their semantics; only wire failures use Transport.
#[derive(Debug)]
pub enum FsError {
    Unsupported(&'static str),
    Denied,
    Revoked,
    Closed,
    StaleEntry,
    StalePage,
    NotFound,
    InvalidName,
    Conflict,
    Busy,
    Cancelled,
    LimitExceeded,
    StoreUnavailable,
    /// Owns the original receipt-publication failure and any commit obligation.
    /// Retrying its commit requires retaining the `OperationOwner`'s writer
    /// exclusion; it neither unpoisons the owner nor resumes job bookkeeping.
    Publication(tilo_files::native::atomic::PublicationError),
    Transport(runtime_portal::NotAvailable),
    Io(std::io::Error),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Publication(error) => write!(f, "receipt publication: {error}"),
            Self::Io(error) => write!(f, "filesystem I/O: {error}"),
            Self::Transport(error) => write!(f, "filesystem transport: {error}"),
            other => write!(f, "filesystem: {other:?}"),
        }
    }
}

impl std::error::Error for FsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Publication(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct FsEntry {
    pub name: EntryName,
    pub grant: GrantedEntry,
    pub metadata: crate::Metadata,
}

/// Opaque continuation of a retained snapshot, bound to one session and directory.
#[derive(Clone, Debug)]
pub struct PageCursor {
    pub(super) generation: u64,
    pub(super) offset: usize,
}

#[derive(Debug)]
pub struct DirectoryPage {
    pub entries: Vec<FsEntry>,
    pub next: Option<PageCursor>,
    pub generation: u64,
}
