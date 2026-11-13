//! Credential-free browsing and streamed reads for independent cloud imports,
//! available only on the cloud Files socket.

use serde::{Deserialize, Serialize};

/// Maximum bytes in a transfer response, below the cloud frame limit.
pub const IMPORT_CHUNK_LIMIT: u32 = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportRequest {
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileRequest {
    Revisions {
        path: String,
    },
    RestoreRevision {
        path: String,
        revision: String,
    },
    ShareLink {
        path: String,
    },
    /// Cached source metadata only; never fetches file content or exports.
    Describe {
        path: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRevision {
    pub revision: String,
    pub modified_unix: Option<i64>,
    pub size: Option<u64>,
    pub author: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDescription {
    pub source_mime: Option<String>,
    pub converted: bool,
    pub content_writable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileReply {
    Revisions(Vec<FileRevision>),
    Restored,
    Link(String),
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
