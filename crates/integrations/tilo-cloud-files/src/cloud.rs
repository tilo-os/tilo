//! Credential-free cloud file requests, available only on the cloud Files
//! socket.

use serde::{Deserialize, Serialize};

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
    Description(FileDescription),
}

/// A mount's health, as it last reported it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MountHealth {
    Syncing,
    Healthy,
    Offline,
    NeedsAuthentication,
    Error,
}

/// What a mounted account's provider offers beyond files and folders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudCapabilities {
    pub revisions: bool,
    pub share_links: bool,
}

/// One mounted account. Carries no credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudMount {
    pub account: String,
    pub mount_point: std::path::PathBuf,
    pub health: Option<MountHealth>,
    pub paused: bool,
    pub capabilities: CloudCapabilities,
    #[serde(default)]
    pub signed_in: bool,
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
    pub fn mounts(&mut self) -> std::io::Result<Vec<CloudMount>> {
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
