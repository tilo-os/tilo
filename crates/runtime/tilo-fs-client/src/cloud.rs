//! Typed control protocol of the per-user cloud account service.
//!
//! `fs cloud-service` keeps one mount per enabled cloud storage account at
//! `~/Remote/<account label>` and serves this protocol on two sockets, each
//! mode 0600 in its own 0700 runtime directory. Settings and Files share
//! these types: a client lists the mounts with their health, capabilities
//! and whether they are paused; asks one to sync now, pause or resume; and
//! asks a mount for a file's provider revisions, to restore one of them or
//! for a share link.
//!
//! The sockets differ in what they serve ([`CloudSocket`]). The account
//! socket, [`default_socket`], answers the listing, sync now, pause, resume
//! and the prepare/abort import handshake: it is what Settings' sandbox is granted. The
//! file socket, [`default_files_socket`], also answers file requests, which
//! can replace a file's remote content or make it reachable by anyone
//! holding a link; it lives in a directory of its own, so a grant of the
//! account socket's directory never reaches it.
//!
//! Every message, in both directions, is one frame: a little-endian `u32`
//! length, then the postcard-encoded [`CloudRequest`] or [`CloudResponse`].
//! The same [`FileRequest`] and [`FileReply`] also travel, as JSON lines,
//! between the service and its mount processes.
//!
//! No credential crosses this socket: a mount holds its account's one
//! short-lived credential and nothing here carries or returns it. A share
//! link is user data, not a secret, but nobody logs it.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wire_frame::{FrameError, Framing};

pub mod import;
pub use import::{
    IMPORT_CHUNK_LIMIT, ImportEntry, ImportError, ImportErrorKind, ImportKind, ImportReadState,
    ImportReply, ImportRequest,
};

/// Every cloud-socket message is one frame of at most 1 MiB.
// A 256 KiB byte-vector chunk can require four JSON bytes per source byte,
// plus its response envelope. Leave room for the envelope at the chunk limit.
pub const CLOUD_FRAMING: Framing = Framing::little_endian(2 * 1024 * 1024);

/// The service's runtime directory below `$XDG_RUNTIME_DIR` (the user
/// unit's `RuntimeDirectory=`).
pub const RUNTIME_DIR_NAME: &str = "tilo-fs-cloud";
/// The socket's file name inside [`RUNTIME_DIR_NAME`].
pub const SOCKET_NAME: &str = "control.sock";
/// The file socket's runtime directory below `$XDG_RUNTIME_DIR`: a sibling
/// of [`RUNTIME_DIR_NAME`], never inside it.
pub const FILES_RUNTIME_DIR_NAME: &str = "tilo-fs-cloud-files";
/// The file socket's name inside [`FILES_RUNTIME_DIR_NAME`].
pub const FILES_SOCKET_NAME: &str = "files.sock";

/// How long a client waits for an answer. Revisions and share links are a
/// provider round trip; the service answers the rest at once.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

/// `$XDG_RUNTIME_DIR/tilo-fs-cloud/control.sock`, or `None` without a
/// runtime directory.
#[must_use]
pub fn default_socket() -> Option<PathBuf> {
    let runtime = tilo_paths::xdg::runtime_dir()?;
    Some(socket_in(&runtime))
}

/// The socket below the runtime directory `runtime_dir`.
#[must_use]
pub fn socket_in(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(RUNTIME_DIR_NAME).join(SOCKET_NAME)
}

/// `$XDG_RUNTIME_DIR/tilo-fs-cloud-files/files.sock`, or `None` without a
/// runtime directory.
#[must_use]
pub fn default_files_socket() -> Option<PathBuf> {
    let runtime = tilo_paths::xdg::runtime_dir()?;
    Some(files_socket_in(&runtime))
}

/// The file socket below the runtime directory `runtime_dir`.
#[must_use]
pub fn files_socket_in(runtime_dir: &Path) -> PathBuf {
    runtime_dir
        .join(FILES_RUNTIME_DIR_NAME)
        .join(FILES_SOCKET_NAME)
}

/// Which of the service's sockets a connection arrived on, and so what it
/// may ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloudSocket {
    /// The account socket: listing, sync controls and import preparation. No
    /// request on it reads, replaces or shares a file.
    Accounts,
    /// The file socket: everything, [`CloudRequest::File`] included.
    Files,
}

impl CloudSocket {
    /// Whether this socket answers `request`; the service refuses any other
    /// before it reaches a mount.
    #[must_use]
    pub fn serves(self, request: &CloudRequest) -> bool {
        match self {
            Self::Files => true,
            Self::Accounts => !matches!(
                request,
                CloudRequest::File { .. } | CloudRequest::Import { .. }
            ),
        }
    }
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

impl MountHealth {
    /// The word the cloud child reports this health with.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Syncing => "syncing",
            Self::Healthy => "healthy",
            Self::Offline => "offline",
            Self::NeedsAuthentication => "needs-authentication",
            Self::Error => "error",
        }
    }

    /// The health [`Self::as_str`] wrote.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "syncing" => Self::Syncing,
            "healthy" => Self::Healthy,
            "offline" => Self::Offline,
            "needs-authentication" => Self::NeedsAuthentication,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

/// What a mounted account's provider offers beyond files and folders. An
/// operation a provider lacks is not offered, rather than offered and
/// failing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudCapabilities {
    /// The provider keeps file revisions: list and restore them.
    pub revisions: bool,
    /// The provider creates share links.
    pub share_links: bool,
}

/// One mounted account. Carries no credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudMount {
    /// The Settings account id.
    pub account: String,
    /// Where the account's folder is mounted (`~/Remote/<label>`).
    pub mount_point: PathBuf,
    /// The last health the mount reported; `None` before its first report.
    pub health: Option<MountHealth>,
    /// Background sync and uploads are paused.
    pub paused: bool,
    pub capabilities: CloudCapabilities,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloudRequest {
    /// Every mounted account.
    List,
    /// Sync the account now: cut a failure back-off short and run a pass;
    /// an account whose mount could not start is tried again.
    SyncNow { account: String },
    /// Stop the account's background sync and uploads until resumed. Reads
    /// still fetch on demand; local edits queue.
    Pause { account: String },
    /// Resume a paused account and sync it now.
    Resume { account: String },
    /// An operation on one file of the account.
    File {
        account: String,
        request: FileRequest,
    },
    /// Browse and read an enabled account without creating a sync mount.
    Import {
        account: String,
        request: ImportRequest,
    },
    /// Freeze a clean sync mount before committing import-only configuration.
    PrepareImport { account: String },
    /// Roll back an uncommitted preparation. Tokens are account/configuration-bound.
    AbortImport { account: String, token: String },
    /// Pin a prepared hold durably before writing account metadata.
    BeginCommitImport { account: String, token: String },
    /// Verify the saved Import mode and consume its pinned hold.
    CompleteImport { account: String, token: String },
    /// Explicitly recover an interrupted commit, only without an active writer.
    RecoverImport { account: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloudResponse {
    Mounts(Vec<CloudMount>),
    Done,
    File(FileReply),
    Error(String),
    Import(ImportReply),
    ImportPrepared { token: String },
}

/// Fence an account metadata writer against explicit interrupted-commit recovery.
/// Hold this guard from preparation through save and completion. Lock files are
/// stable and must not be removed, including after a daemon restart.
pub fn import_writer_lock(
    config_root: &Path,
    account: &str,
) -> std::io::Result<advisory_lock::FileLock> {
    use std::fmt::Write;
    let mut name = String::new();
    for byte in account.as_bytes() {
        let _ = write!(name, "{byte:02x}");
    }
    let directory = config_root.join("cloud-transition-locks");
    std::fs::create_dir_all(&directory)?;
    advisory_lock::FileLock::try_open(&directory.join(name), advisory_lock::LockMode::Exclusive)
        .map_err(|error| std::io::Error::other(error.to_string()))
}

/// A connection to the cloud account service.
pub struct CloudClient {
    stream: UnixStream,
}

impl CloudClient {
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if nothing listens at `path`.
    pub fn connect(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        stream.set_write_timeout(Some(REQUEST_TIMEOUT))?;
        Ok(Self { stream })
    }

    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the exchange fails or the service
    /// answers with an error.
    pub fn mounts(&mut self) -> std::io::Result<Vec<CloudMount>> {
        match self.request(&CloudRequest::List)? {
            CloudResponse::Mounts(mounts) => Ok(mounts),
            other => Err(unexpected(other)),
        }
    }

    /// Browse or transfer bytes without exposing provider credentials.
    pub fn import(
        &mut self,
        account: &str,
        request: ImportRequest,
    ) -> std::io::Result<ImportReply> {
        match self.request(&CloudRequest::Import {
            account: account.to_owned(),
            request,
        })? {
            CloudResponse::Import(reply) => Ok(reply),
            other => Err(unexpected(other)),
        }
    }

    /// Freeze and stop a clean sync mount before changing its account mode.
    /// Begin the commit within 120 seconds, or abort with the token.
    pub fn prepare_import(&mut self, account: &str) -> std::io::Result<String> {
        match self.request(&CloudRequest::PrepareImport {
            account: account.to_owned(),
        })? {
            CloudResponse::ImportPrepared { token } => Ok(token),
            other => Err(unexpected(other)),
        }
    }

    /// Restore a prepared mount after an account-store write fails.
    pub fn abort_import(&mut self, account: &str, token: &str) -> std::io::Result<()> {
        self.control(&CloudRequest::AbortImport {
            account: account.to_owned(),
            token: token.to_owned(),
        })
    }

    /// Pin a hold across metadata persistence, including service restarts.
    pub fn begin_commit_import(&mut self, account: &str, token: &str) -> std::io::Result<()> {
        self.control(&CloudRequest::BeginCommitImport {
            account: account.to_owned(),
            token: token.to_owned(),
        })
    }

    /// Consume a pinned hold after account metadata was saved.
    ///
    /// # Errors
    /// Returns an error unless the token matches and the account is in Import mode.
    pub fn complete_import(&mut self, account: &str, token: &str) -> std::io::Result<()> {
        self.control(&CloudRequest::CompleteImport {
            account: account.to_owned(),
            token: token.to_owned(),
        })
    }

    /// Send `request`, which answers [`CloudResponse::Done`].
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the exchange fails or the service
    /// answers with an error.
    pub fn control(&mut self, request: &CloudRequest) -> std::io::Result<()> {
        match self.request(request)? {
            CloudResponse::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the exchange fails, or the service
    /// or the provider refuses the operation.
    pub fn file(&mut self, account: &str, request: FileRequest) -> std::io::Result<FileReply> {
        let request = CloudRequest::File {
            account: account.to_owned(),
            request,
        };
        match self.request(&request)? {
            CloudResponse::File(reply) => Ok(reply),
            other => Err(unexpected(other)),
        }
    }

    /// One exchange.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the write or read fails or the
    /// service closed the connection.
    pub fn request(&mut self, request: &CloudRequest) -> std::io::Result<CloudResponse> {
        CLOUD_FRAMING.write_value(&mut self.stream, request)?;
        Ok(CLOUD_FRAMING
            .read_value(&mut self.stream)?
            .ok_or(FrameError::Closed)?)
    }
}

fn unexpected(response: CloudResponse) -> std::io::Error {
    match response {
        CloudResponse::Error(error) => std::io::Error::other(error),
        _ => std::io::Error::other("unexpected cloud service response"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_wire_round_trips_and_errors_surface() {
        let (client_end, mut service) = UnixStream::pair().unwrap();
        let answer = std::thread::spawn(move || {
            let request: CloudRequest = CLOUD_FRAMING.read_value(&mut service).unwrap().unwrap();
            assert_eq!(
                request,
                CloudRequest::File {
                    account: "a".into(),
                    request: FileRequest::ShareLink {
                        path: "x.txt".into()
                    },
                }
            );
            CLOUD_FRAMING
                .write_value(&mut service, &CloudResponse::Error("no".into()))
                .unwrap();
            let request: CloudRequest = CLOUD_FRAMING.read_value(&mut service).unwrap().unwrap();
            assert_eq!(request, CloudRequest::List);
            let mounts = vec![CloudMount {
                account: "a".into(),
                mount_point: "/home/ada/Remote/A".into(),
                health: Some(MountHealth::Offline),
                paused: true,
                capabilities: CloudCapabilities {
                    revisions: true,
                    share_links: false,
                },
            }];
            CLOUD_FRAMING
                .write_value(&mut service, &CloudResponse::Mounts(mounts))
                .unwrap();
        });
        let mut client = CloudClient { stream: client_end };
        let refused = client
            .file(
                "a",
                FileRequest::ShareLink {
                    path: "x.txt".into(),
                },
            )
            .unwrap_err();
        assert_eq!(refused.to_string(), "no");
        let mounts = client.mounts().unwrap();
        answer.join().unwrap();
        assert_eq!(mounts.len(), 1);
        assert!(mounts[0].paused);
        assert_eq!(mounts[0].health, Some(MountHealth::Offline));
        assert!(mounts[0].capabilities.revisions);
    }

    #[test]
    fn describe_round_trips_on_the_file_socket() {
        let (client_end, mut service) = UnixStream::pair().unwrap();
        let request = CloudRequest::File {
            account: "a".into(),
            request: FileRequest::Describe {
                path: "Work/Essay.docx".into(),
            },
        };
        assert!(!CloudSocket::Accounts.serves(&request));
        assert!(CloudSocket::Files.serves(&request));
        let description = FileDescription {
            source_mime: Some("application/vnd.google-apps.document".into()),
            converted: true,
            content_writable: false,
        };
        let reply = FileReply::Description(description);
        let expected = reply.clone();
        let worker = std::thread::spawn(move || {
            let received: CloudRequest = CLOUD_FRAMING.read_value(&mut service).unwrap().unwrap();
            assert_eq!(received, request);
            CLOUD_FRAMING
                .write_value(&mut service, &CloudResponse::File(reply))
                .unwrap();
        });
        let mut client = CloudClient { stream: client_end };
        assert_eq!(
            client
                .file(
                    "a",
                    FileRequest::Describe {
                        path: "Work/Essay.docx".into()
                    }
                )
                .unwrap(),
            expected
        );
        worker.join().unwrap();
    }

    #[test]
    fn maximum_import_chunk_fits_the_wire_frame() {
        let (client_end, mut service) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let request: CloudRequest = CLOUD_FRAMING.read_value(&mut service).unwrap().unwrap();
            assert!(matches!(
                request,
                CloudRequest::Import {
                    request: ImportRequest::ReadChunk {
                        length: IMPORT_CHUNK_LIMIT,
                        ..
                    },
                    ..
                }
            ));
            CLOUD_FRAMING
                .write_value(
                    &mut service,
                    &CloudResponse::Import(ImportReply::Chunk {
                        bytes: vec![255; IMPORT_CHUNK_LIMIT as usize],
                        eof: true,
                    }),
                )
                .unwrap();
        });
        let mut client = CloudClient { stream: client_end };
        let reply = client
            .import(
                "account",
                ImportRequest::ReadChunk {
                    session: "opaque".into(),
                    offset: 0,
                    length: IMPORT_CHUNK_LIMIT,
                },
            )
            .unwrap();
        assert_eq!(
            reply,
            ImportReply::Chunk {
                bytes: vec![255; IMPORT_CHUNK_LIMIT as usize],
                eof: true
            }
        );
        worker.join().unwrap();
    }

    #[test]
    fn the_socket_lives_in_the_service_runtime_directory() {
        assert_eq!(
            socket_in(Path::new("/run/user/1000")),
            Path::new("/run/user/1000/tilo-fs-cloud/control.sock")
        );
        let files = files_socket_in(Path::new("/run/user/1000"));
        assert_eq!(
            files,
            Path::new("/run/user/1000/tilo-fs-cloud-files/files.sock")
        );
        assert!(
            !files.starts_with(Path::new("/run/user/1000").join(RUNTIME_DIR_NAME)),
            "a grant of the account socket's directory must not reach the file socket"
        );
    }

    #[test]
    fn only_the_file_socket_serves_file_requests() {
        let file = CloudRequest::File {
            account: "a".into(),
            request: FileRequest::ShareLink {
                path: "x.txt".into(),
            },
        };
        assert!(!CloudSocket::Accounts.serves(&file));
        assert!(CloudSocket::Files.serves(&file));
        let account = "a".to_owned();
        for request in [
            CloudRequest::List,
            CloudRequest::SyncNow {
                account: account.clone(),
            },
            CloudRequest::Pause {
                account: account.clone(),
            },
            CloudRequest::Resume {
                account: account.clone(),
            },
            CloudRequest::PrepareImport {
                account: account.clone(),
            },
            CloudRequest::AbortImport {
                account: account.clone(),
                token: "opaque".into(),
            },
            CloudRequest::BeginCommitImport {
                account: account.clone(),
                token: "opaque".into(),
            },
            CloudRequest::CompleteImport {
                account: account.clone(),
                token: "opaque".into(),
            },
            CloudRequest::RecoverImport { account },
        ] {
            assert!(CloudSocket::Accounts.serves(&request));
            assert!(CloudSocket::Files.serves(&request));
        }
    }
}
