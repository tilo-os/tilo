//! Opaque, live filesystem authority. Paths and names never confer a grant.

pub use tilo_fs_client::{EntryName, FsOwner, FsRight};

use crate::native::GrantLease;

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

impl From<tilo_fs_client::InvalidEntryName> for FsError {
    fn from(_: tilo_fs_client::InvalidEntryName) -> Self {
        Self::InvalidName
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
