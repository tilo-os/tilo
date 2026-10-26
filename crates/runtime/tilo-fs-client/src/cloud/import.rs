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
