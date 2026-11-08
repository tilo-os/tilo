//! Clients consume host-admitted capabilities, never native grant authority.
//!
//! A client drives its connection on a thread of its own, so it needs no
//! runtime context and calls from any thread share one transport.
//!
//! ```compile_fail
//! use tilo_fs_client::NativeGrantAuthority;
//! ```

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(unix)]
use std::sync::Arc;

use runtime_portal::NotAvailable;

#[cfg(unix)]
use crate::connection::{Connection, Object};
#[cfg(unix)]
use crate::wire::{FsReply, FsRequest};

#[cfg(unix)]
mod bridge;
#[cfg(unix)]
pub mod connection;
#[cfg(unix)]
pub mod directory_client;
#[cfg(unix)]
pub mod directory_connection;
#[cfg(unix)]
pub mod directory_types;
#[cfg(unix)]
pub mod grants;
#[cfg(unix)]
pub use grants::{FsOwner, FsRight};
#[cfg(unix)]
mod entry_name;
#[cfg(unix)]
pub mod external_history;
#[cfg(unix)]
pub mod selected_client;
#[cfg(unix)]
pub use entry_name::{EntryName, InvalidEntryName};
#[cfg(unix)]
pub mod blobs;
#[cfg(unix)]
pub mod cloud;
pub mod native_reply;
pub mod storage;
#[cfg(unix)]
pub mod wire;

pub const SELECTED_FILE: &str = "file";

/// Byte cap the server-side `FileHandle` applies to a single `read`; larger
/// requests are truncated. Mirrors the FUSE driver's 1 MiB readahead.
pub const MAX_READ_CHUNK: u32 = 1 << 20;

/// Upper bound of one in-place held-file write syscall. Larger requests refuse.
pub const MAX_WRITE_CHUNK: u32 = 1 << 20;

/// Try to dial the Tilo workspace-fs proxy via the launcher-supplied service
/// fd. Returns [`NotAvailable`] when the env var is unset (e.g. running as
/// a Flatpak) or the fd isn't open. A process has one connection to the
/// service, so a second dial reports it as taken.
///
/// # Errors
///
/// Returns [`NotAvailable::ServiceFdMissing`] if the launcher didn't pass a
/// service fd, or [`NotAvailable::ConnectionFailed`] if the fd can't be
/// lifted to a connection.
#[cfg(unix)]
pub fn try_connect() -> Result<FsClient, NotAvailable> {
    let fd = runtime_portal::connect_service_fd("fs")?;
    connect_stream(fd.into())
}

/// Outside a Tilo session there is no workspace filesystem to dial.
/// # Errors
/// Always returns [`NotAvailable::ServiceFdMissing`].
#[cfg(not(unix))]
pub fn try_connect() -> Result<FsClient, NotAvailable> {
    Err(NotAvailable::ServiceFdMissing)
}

/// Dial a workspace a host serves at `socket`
/// (the host's workspace server).
///
/// # Errors
///
/// Returns [`NotAvailable::ConnectionFailed`] when nothing serves `socket`.
#[cfg(unix)]
pub fn connect(socket: &std::path::Path) -> Result<FsClient, NotAvailable> {
    connect_stream(std::os::unix::net::UnixStream::connect(socket)?)
}

/// Connect the existing filesystem protocol over an already-admitted stream.
/// The trusted host supplies the live endpoint; this function does not mint a grant.
/// # Errors
/// Refuses a disconnected descriptor or protocol initialization failure.
#[cfg(unix)]
pub fn connect_stream(stream: std::os::unix::net::UnixStream) -> Result<FsClient, NotAvailable> {
    let connection =
        Connection::connect("workspace-fs", stream).map_err(NotAvailable::ConnectionFailed)?;
    Ok(FsClient {
        object: Object::root(connection),
    })
}

#[cfg(unix)]
/// File open mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OpenMode {
    /// Read-only.
    Read,
    /// Truncate + write. Creates the file if missing.
    Write,
    /// Append. Creates the file if missing.
    Append,
    /// Read and positioned write of an existing file; never create or truncate.
    ReadWriteExisting,
    /// Positioned write of an existing inode; never create, truncate or read.
    WriteExisting,
}

#[cfg(unix)]
/// Directory-entry kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    /// Anything else (socket, fifo, block/char device, or a future kind).
    Other,
}

#[cfg(unix)]
/// A path's metadata snapshot.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Metadata {
    pub kind: EntryKind,
    pub size: u64,
    /// Last-modified time, seconds since epoch; `None` when the server does not know it.
    pub modified_unix_seconds: Option<i64>,
    /// True when the FUSE driver knows about the file but hasn't hydrated its
    /// bytes locally; a read triggers hydration transparently.
    pub needs_hydration: bool,
}

#[cfg(unix)]
/// A directory row — `name` is the last path component, metadata pre-fetched.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub metadata: Metadata,
}

#[cfg(unix)]
/// A workspace filesystem capability.
#[derive(Clone)]
pub struct FsClient {
    pub(crate) object: Arc<Object>,
}

impl FsClient {
    /// The workspace capability rooted at an already connected object.
    pub fn from_object(object: Arc<Object>) -> Self {
        Self { object }
    }

    /// Open independently admitted native directory custody, preserving raw names.
    /// # Errors
    /// Refuses backends without a directory grant and lost or revoked connections.
    pub async fn directory(&self) -> Result<directory_client::DirectoryClient, NotAvailable> {
        match self.object.call(FsRequest::Directory).await? {
            FsReply::Handle(handle) => Ok(directory_client::DirectoryClient::from_object(
                handle_reply(handle, &self.object)?,
            )),
            _ => Err(unexpected()),
        }
    }

    /// Open a file; `path` is workspace-relative, a leading `/` is tolerated.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::Refused`] if `path` doesn't exist or the server
    /// rejects the request, and [`NotAvailable::ConnectionFailed`] if the
    /// connection drops before a reply arrives.
    pub async fn open(&self, path: &str, mode: OpenMode) -> Result<FileHandle, NotAvailable> {
        match self
            .object
            .call(FsRequest::Open {
                path: path.to_owned(),
                mode,
            })
            .await?
        {
            FsReply::Handle(handle) => Ok(FileHandle {
                object: handle_reply(handle, &self.object)?,
            }),
            _ => Err(unexpected()),
        }
    }

    /// List a directory. Snapshot semantics: the vec is the entries at the
    /// moment of the call.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::Refused`] if `path` isn't a directory, and
    /// [`NotAvailable::ConnectionFailed`] if the connection drops before a
    /// reply arrives.
    pub async fn list(&self, path: &str) -> Result<Vec<DirEntry>, NotAvailable> {
        match self
            .object
            .call(FsRequest::List {
                path: path.to_owned(),
            })
            .await?
        {
            FsReply::Entries(entries) => entries.ok_or_else(crate::native_reply::refused),
            _ => Err(unexpected()),
        }
    }

    /// Stat a single path; a missing path surfaces as [`NotAvailable::Refused`].
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::Refused`] if `path` doesn't exist, and
    /// [`NotAvailable::ConnectionFailed`] if the connection drops before a
    /// reply arrives.
    pub async fn stat(&self, path: &str) -> Result<Metadata, NotAvailable> {
        match self
            .object
            .call(FsRequest::Stat {
                path: path.to_owned(),
            })
            .await?
        {
            FsReply::Metadata(metadata) => metadata.ok_or_else(crate::native_reply::refused),
            _ => Err(unexpected()),
        }
    }

    /// Create a directory (with intermediate mkdirs); idempotent if it already exists.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::Refused`] if the server rejects the request
    /// (e.g. a path component exists and isn't a directory), and
    /// [`NotAvailable::ConnectionFailed`] if the connection drops before a
    /// reply arrives.
    pub async fn mkdir(&self, path: &str) -> Result<(), NotAvailable> {
        match self
            .object
            .call(FsRequest::Mkdir {
                path: path.to_owned(),
            })
            .await?
        {
            FsReply::Empty => Ok(()),
            _ => Err(unexpected()),
        }
    }

    /// Remove a file (unlink) or an empty directory (rmdir).
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::Refused`] if `path` doesn't exist or a
    /// directory at `path` isn't empty, and [`NotAvailable::ConnectionFailed`]
    /// if the connection drops before a reply arrives.
    pub async fn remove(&self, path: &str) -> Result<(), NotAvailable> {
        match self
            .object
            .call(FsRequest::Remove {
                path: path.to_owned(),
            })
            .await?
        {
            FsReply::Empty => Ok(()),
            _ => Err(unexpected()),
        }
    }
}

#[cfg(unix)]
/// An open file; dropping the last handle closes it on the server.
#[derive(Clone)]
pub struct FileHandle {
    pub(crate) object: Arc<Object>,
}

impl FileHandle {
    /// # Errors
    /// Refuses absent authentic scope checking, revocation or transport failure.
    pub async fn check_scope(&self) -> Result<(), NotAvailable> {
        match self.object.call(FsRequest::CheckScope).await? {
            FsReply::Empty => Ok(()),
            _ => Err(unexpected()),
        }
    }

    /// Read up to `max_len` bytes at `offset`; the server caps it at [`MAX_READ_CHUNK`].
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::Refused`] if the handle is no longer valid, and
    /// [`NotAvailable::ConnectionFailed`] if the connection drops before a
    /// reply arrives.
    pub async fn read_at(&self, offset: u64, max_len: u32) -> Result<Vec<u8>, NotAvailable> {
        let bytes = match self
            .object
            .call(FsRequest::Read { offset, max_len })
            .await?
        {
            FsReply::Bytes(bytes) => bytes.ok_or_else(crate::native_reply::refused)?,
            _ => return Err(unexpected()),
        };
        if bytes.len() > MAX_READ_CHUNK as usize || bytes.len() > max_len as usize {
            return Err(crate::native_reply::refused());
        }
        Ok(bytes)
    }

    /// Read the whole file in [`MAX_READ_CHUNK`]-sized reads until EOF; for
    /// large files prefer [`Self::read_at`] streamed to disk.
    ///
    /// # Errors
    ///
    /// Returns the error of the first underlying [`Self::read_at`] call that
    /// fails.
    pub async fn read_all(&self) -> Result<Vec<u8>, NotAvailable> {
        let mut buf = Vec::new();
        let mut offset: u64 = 0;
        loop {
            let chunk = self.read_at(offset, MAX_READ_CHUNK).await?;
            if chunk.is_empty() {
                return Ok(buf);
            }
            offset = offset.saturating_add(chunk.len() as u64);
            buf.extend_from_slice(&chunk);
        }
    }

    /// Write `bytes` at `offset`; returns the bytes written.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::Refused`] if the handle isn't writable, and
    /// [`NotAvailable::ConnectionFailed`] if the connection drops before a
    /// reply arrives.
    pub async fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<u32, NotAvailable> {
        match self
            .object
            .call(FsRequest::Write {
                offset,
                bytes: bytes.to_vec(),
            })
            .await?
        {
            FsReply::Written(written) => written.ok_or_else(crate::native_reply::refused),
            _ => Err(unexpected()),
        }
    }

    /// Append one bounded chunk atomically. Never retry an unknown outcome.
    /// # Errors
    /// Refuses non-append handles, oversized chunks, revocation, or transport loss.
    pub async fn append(&self, bytes: &[u8]) -> Result<u32, NotAvailable> {
        if bytes.len() > MAX_WRITE_CHUNK as usize {
            return Err(crate::native_reply::refused());
        }
        let written = match self
            .object
            .call(FsRequest::Append {
                bytes: bytes.to_vec(),
            })
            .await?
        {
            FsReply::Written(written) => written.ok_or_else(crate::native_reply::refused)?,
            _ => return Err(unexpected()),
        };
        if written as usize > bytes.len() {
            return Err(crate::native_reply::refused());
        }
        Ok(written)
    }

    /// Persist any buffered writes. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::Refused`] if the server rejects the flush, and
    /// [`NotAvailable::ConnectionFailed`] if the connection drops before a
    /// reply arrives.
    pub async fn flush(&self) -> Result<(), NotAvailable> {
        match self.object.call(FsRequest::Flush).await? {
            FsReply::Ack(status) => crate::native_reply::check(status),
            _ => Err(unexpected()),
        }
    }

    /// Stat this open handle — cheaper than [`FsClient::stat`] when you already have it.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable::Refused`] if the handle is no longer valid, and
    /// [`NotAvailable::ConnectionFailed`] if the connection drops before a
    /// reply arrives.
    pub async fn metadata(&self) -> Result<Metadata, NotAvailable> {
        match self.object.call(FsRequest::HandleMetadata).await? {
            FsReply::Metadata(metadata) => metadata.ok_or_else(crate::native_reply::refused),
            _ => Err(unexpected()),
        }
    }
}

/// A capability reply named an object this end may hold.
#[cfg(unix)]
pub(crate) fn handle_reply(
    handle: Option<u64>,
    served_by: &Arc<Object>,
) -> Result<Arc<Object>, NotAvailable> {
    Ok(Object::child(
        Arc::clone(&served_by.connection),
        handle.ok_or_else(crate::native_reply::refused)?,
    ))
}

/// The reply is not the one this request answers with: the peer is not the
/// filesystem protocol this client speaks.
#[cfg(unix)]
pub(crate) fn unexpected() -> NotAvailable {
    NotAvailable::Refused("unexpected filesystem reply".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::env;

    fn clear_tilo_env() -> tilo_test_env::ScopedEnv {
        let mut scoped = tilo_test_env::ScopedEnv::new();
        for (key, _) in env::vars() {
            if key.starts_with("TILO_") {
                scoped.remove(&key);
            }
        }
        scoped
    }

    #[test]
    fn try_connect_without_env_returns_service_fd_missing() {
        let _env = clear_tilo_env();
        match try_connect() {
            Err(NotAvailable::ServiceFdMissing) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
            Ok(_) => panic!("expected ServiceFdMissing"),
        }
    }
}
