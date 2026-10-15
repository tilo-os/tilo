//! Credential-free browsing and streamed reads for independent cloud imports.
//! These requests are available only on the cloud Files socket.

use serde::{Deserialize, Serialize};

/// Maximum bytes in a transfer response, below the cloud frame limit.
pub const IMPORT_CHUNK_LIMIT: u32 = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportRequest {
    /// Browse the root when `folder` is absent. IDs are opaque provider IDs.
    Browse {
        folder: Option<String>,
        offset: u32,
    },
    /// Prepare verified file/export bytes without blocking the service loop.
    StartRead {
        id: String,
        revision: Option<String>,
    },
    ReadStatus {
        session: String,
    },
    ReadChunk {
        session: String,
        offset: u64,
        length: u32,
    },
    /// Idempotently cancel and release a prepared read.
    CloseRead {
        session: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportKind {
    Folder,
    File,
    NativeExport,
    Unsupported,
}

/// One remote entry as it will be represented locally, never a download URL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportEntry {
    pub id: String,
    pub name: String,
    pub kind: ImportKind,
    pub revision: Option<String>,
    pub size: Option<u64>,
    /// Native converted views do not support safe round-trip content edits.
    pub content_writable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportErrorKind {
    AuthenticationRequired,
    Offline,
    PermissionDenied,
    Unsupported,
    TooLarge,
    SourceChanged,
    Cancelled,
    QuotaExceeded,
    InvalidRequest,
    Other,
}

/// Safe user-facing error; never contains credentials or private cache paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportError {
    pub kind: ImportErrorKind,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportReadState {
    Preparing,
    Ready { size: u64 },
    Failed(ImportError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportReply {
    Page {
        folder: String,
        entries: Vec<ImportEntry>,
        next_offset: Option<u32>,
    },
    Preparing {
        session: String,
    },
    ReadState(ImportReadState),
    Chunk {
        bytes: Vec<u8>,
        eof: bool,
    },
    Closed,
    Error(ImportError),
}

/// An operation on one file of a mounted account. `path` is relative to
/// the account's folder, `/`-separated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileRequest {
    /// The file's provider revisions, newest first as the provider lists
    /// them.
    Revisions { path: String },
    /// Make `revision` the file's current version. Replaces the current
    /// remote content.
    RestoreRevision { path: String, revision: String },
    /// A link the provider shares the file through. Creating one may make
    /// the file reachable by anyone holding the link.
    ShareLink { path: String },
    /// Cached source metadata only; never fetches file content or exports.
    Describe { path: String },
}

/// One revision of a file, as the provider describes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRevision {
    /// The provider's opaque revision id, as [`FileRequest::RestoreRevision`]
    /// takes it.
    pub revision: String,
    pub modified_unix: Option<i64>,
    pub size: Option<u64>,
    pub author: Option<String>,
}

/// Cached metadata about a mounted item. Carries no credential or content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDescription {
    /// The provider's original MIME type, not the exported representation's.
    pub source_mime: Option<String>,
    /// The mounted file represents a supported native-document export.
    pub converted: bool,
    /// The mount permits replacing the item's content (not rename/delete).
    pub content_writable: bool,
}

/// The answer to a [`FileRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileReply {
    Revisions(Vec<FileRevision>),
    Restored,
    Link(String),
    /// Internal mount acknowledgement; no file content is returned.
    ImportFrozen,
    Description(FileDescription),
}

/// No native Files socket is exposed to a browser or a non-Unix host.
pub fn default_files_socket() -> Option<std::path::PathBuf> {
    None
}

/// Unsupported-target client. It never tries a native path or a web URL.
pub struct CloudClient;

impl CloudClient {
    /// # Errors
    /// Always returns Unsupported: the native Files service uses Unix sockets.
    pub fn connect(_: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "cloud Files service requires a Unix host",
        ))
    }
    /// # Errors
    /// Always returns Unsupported.
    pub fn import(&mut self, _: &str, _: ImportRequest) -> std::io::Result<ImportReply> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "cloud Files service requires a Unix host",
        ))
    }
    /// # Errors
    /// Always returns Unsupported.
    pub fn file(&mut self, _: &str, _: FileRequest) -> std::io::Result<FileReply> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "cloud Files service requires a Unix host",
        ))
    }
}
