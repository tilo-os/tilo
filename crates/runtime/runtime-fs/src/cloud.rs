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
//! socket, [`default_socket`], answers the listing and sync now, pause and
//! resume, and nothing else: it is what Settings' sandbox is granted. The
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

/// Every cloud-socket message is one frame of at most 1 MiB.
pub const CLOUD_FRAMING: Framing = Framing::little_endian(1024 * 1024);

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
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())?;
    Some(socket_in(Path::new(&runtime)))
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
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())?;
    Some(files_socket_in(Path::new(&runtime)))
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
    /// The account socket: the listing, sync now, pause and resume. No
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
            Self::Accounts => !matches!(request, CloudRequest::File { .. }),
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

/// The answer to a [`FileRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileReply {
    Revisions(Vec<FileRevision>),
    Restored,
    Link(String),
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloudResponse {
    Mounts(Vec<CloudMount>),
    Done,
    File(FileReply),
    Error(String),
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
            CloudRequest::Resume { account },
        ] {
            assert!(CloudSocket::Accounts.serves(&request));
            assert!(CloudSocket::Files.serves(&request));
        }
    }
}
