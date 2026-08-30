//! `runtime-fs` — Cap'n Proto client for the qOS workspace filesystem proxy.
//!
//! ```ignore
//! let local = tokio::task::LocalSet::new();
//! local.run_until(async {
//!     let fs = runtime_fs::try_connect()
//!         .map_err(|e| { eprintln!("workspace fs unavailable: {e}"); e })?;
//!
//!     let file = fs.open("documents/notes.md", OpenMode::Read).await?;
//!     let bytes = file.read_all().await?;
//!     println!("{} bytes", bytes.len());
//!
//!     let listing = fs.list("documents").await?;
//!     for entry in listing {
//!         println!("{} — {} bytes", entry.name, entry.metadata.size);
//!     }
//! }).await;
//! ```
//!
//! A missing service (env var unset, fd closed, launcher not present)
//! surfaces as [`NotAvailable::ServiceFdMissing`] rather than a panic; apps
//! are expected to fall back to a stock-Linux path (or disable the
//! feature) when running as a Flatpak.

use std::rc::Rc;

use runtime_portal::{NotAvailable, UnixCapnpClient, service_fd};

// Generated schema. `include!` at the crate root under the exact
// module name the capnp compiler emits (`fs_capnp` for schema/fs.capnp).
#[allow(clippy::all)]
#[allow(unused)]
#[allow(dead_code)]
pub mod fs_capnp {
    include!(concat!(env!("OUT_DIR"), "/fs_capnp.rs"));
}

use capnp_rpc::rpc_twoparty_capnp;

pub mod storage;

pub use fs_capnp::workspace_fs::Client as WorkspaceFsClient;

/// Byte cap the server-side `FileHandle` applies to a single `read`; larger
/// requests are truncated. Mirrors the FUSE driver's 1 MiB readahead.
pub const MAX_READ_CHUNK: u32 = 1 << 20;

/// Try to dial the qOS workspace-fs proxy via the launcher-supplied service
/// fd. Returns [`NotAvailable`] when the env var is unset (e.g. running as
/// a Flatpak) or the fd isn't open. Call from a `tokio::task::LocalSet` —
/// capnp-rpc futures are `!Send`.
pub fn try_connect() -> Result<FsClient, NotAvailable> {
    let fd = service_fd("fs").ok_or(NotAvailable::ServiceFdMissing)?;
    let unix = UnixCapnpClient::from_connected_fd(fd, rpc_twoparty_capnp::Side::Client)?;
    let client: WorkspaceFsClient = unix.bootstrap();
    Ok(FsClient::from_raw(client))
}

/// File open mode. Mirrors the `OpenMode` enum in the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Read-only.
    Read,
    /// Truncate + write. Creates the file if missing.
    Write,
    /// Append. Creates the file if missing.
    Append,
}

impl From<OpenMode> for fs_capnp::OpenMode {
    fn from(m: OpenMode) -> Self {
        match m {
            OpenMode::Read => fs_capnp::OpenMode::Read,
            OpenMode::Write => fs_capnp::OpenMode::Write,
            OpenMode::Append => fs_capnp::OpenMode::Append,
        }
    }
}

/// Directory-entry kind. Mirrors the `EntryKind` enum in the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    /// Anything else (socket, fifo, block/char device, or a future schema value).
    Other,
}

impl From<fs_capnp::EntryKind> for EntryKind {
    fn from(k: fs_capnp::EntryKind) -> Self {
        match k {
            fs_capnp::EntryKind::File => EntryKind::File,
            fs_capnp::EntryKind::Directory => EntryKind::Directory,
            fs_capnp::EntryKind::Symlink => EntryKind::Symlink,
            fs_capnp::EntryKind::Other => EntryKind::Other,
        }
    }
}

/// A path's metadata snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub kind: EntryKind,
    pub size: u64,
    /// Last-modified time, seconds since epoch; `None` when the server reported `0` (unknown).
    pub modified_unix_seconds: Option<i64>,
    /// True when the FUSE driver knows about the file but hasn't hydrated its
    /// bytes locally; a read triggers hydration transparently.
    pub needs_hydration: bool,
}

impl Metadata {
    fn from_reader(r: fs_capnp::metadata::Reader<'_>) -> Result<Self, NotAvailable> {
        let secs = r.get_modified_unix_seconds();
        // Funnel `get_kind()`'s NotInSchema through capnp::Error for a uniform `?`;
        // an unknown wire value collapses to `Other` since schema evolution must
        // not break clients.
        let kind = match r.get_kind() {
            Ok(k) => k.into(),
            Err(_) => EntryKind::Other,
        };
        Ok(Self {
            kind,
            size: r.get_size(),
            modified_unix_seconds: if secs == 0 { None } else { Some(secs) },
            needs_hydration: r.get_needs_hydration(),
        })
    }
}

/// A directory row — `name` is the last path component, metadata pre-fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub metadata: Metadata,
}

/// Idiomatic Rust wrapper over the generated [`WorkspaceFsClient`].
#[derive(Clone)]
pub struct FsClient {
    inner: Rc<WorkspaceFsClient>,
}

impl FsClient {
    /// Wrap an existing generated client (mainly for tests with their own RPC system).
    pub fn from_raw(client: WorkspaceFsClient) -> Self {
        Self {
            inner: Rc::new(client),
        }
    }

    /// Open a file; `path` is workspace-relative, a leading `/` is tolerated.
    pub async fn open(&self, path: &str, mode: OpenMode) -> Result<FileHandle, NotAvailable> {
        let mut req = self.inner.open_request();
        {
            let mut b = req.get();
            b.set_path(path);
            b.set_mode(mode.into());
        }
        let reply = req.send().promise.await?;
        let handle = reply.get()?.get_handle()?;
        Ok(FileHandle {
            inner: Rc::new(handle),
        })
    }

    /// List a directory. Snapshot semantics: the vec is the entries at the
    /// moment of the call — re-list or (once available) watch for changes.
    pub async fn list(&self, path: &str) -> Result<Vec<DirEntry>, NotAvailable> {
        let mut req = self.inner.list_request();
        req.get().set_path(path);
        let reply = req.send().promise.await?;
        let reader = reply.get()?;
        let entries = reader.get_entries()?;
        let mut out = Vec::with_capacity(entries.len() as usize);
        for entry in entries {
            let name = entry
                .get_name()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("invalid utf8 name: {e}")))?
                .to_owned();
            let metadata = Metadata::from_reader(entry.get_metadata()?)?;
            out.push(DirEntry { name, metadata });
        }
        Ok(out)
    }

    /// Stat a single path; a missing path surfaces as [`NotAvailable::Capnp`].
    pub async fn stat(&self, path: &str) -> Result<Metadata, NotAvailable> {
        let mut req = self.inner.stat_request();
        req.get().set_path(path);
        let reply = req.send().promise.await?;
        Metadata::from_reader(reply.get()?.get_metadata()?)
    }

    /// Create a directory (with intermediate mkdirs); idempotent if it already exists.
    pub async fn mkdir(&self, path: &str) -> Result<(), NotAvailable> {
        let mut req = self.inner.mkdir_request();
        req.get().set_path(path);
        req.send().promise.await?;
        Ok(())
    }

    /// Remove a file (unlink) or an empty directory (rmdir).
    pub async fn remove(&self, path: &str) -> Result<(), NotAvailable> {
        let mut req = self.inner.remove_request();
        req.get().set_path(path);
        req.send().promise.await?;
        Ok(())
    }
}

/// An open file; dropping the handle closes it on the server.
#[derive(Clone)]
pub struct FileHandle {
    inner: Rc<fs_capnp::file_handle::Client>,
}

impl FileHandle {
    /// Read up to `max_len` bytes at `offset`; the server caps it at [`MAX_READ_CHUNK`].
    pub async fn read_at(&self, offset: u64, max_len: u32) -> Result<Vec<u8>, NotAvailable> {
        let mut req = self.inner.read_request();
        {
            let mut b = req.get();
            b.set_offset(offset);
            b.set_max_len(max_len);
        }
        let reply = req.send().promise.await?;
        Ok(reply.get()?.get_bytes()?.to_vec())
    }

    /// Read the whole file in [`MAX_READ_CHUNK`]-sized reads until EOF; for
    /// large files prefer [`Self::read_at`] streamed to disk.
    pub async fn read_all(&self) -> Result<Vec<u8>, NotAvailable> {
        let mut buf = Vec::new();
        let mut offset: u64 = 0;
        loop {
            let chunk = self.read_at(offset, MAX_READ_CHUNK).await?;
            if chunk.is_empty() {
                return Ok(buf);
            }
            offset += chunk.len() as u64;
            buf.extend_from_slice(&chunk);
        }
    }

    /// Write `bytes` at `offset`; returns the bytes written.
    pub async fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<u32, NotAvailable> {
        let mut req = self.inner.write_request();
        {
            let mut b = req.get();
            b.set_offset(offset);
            b.set_bytes(bytes);
        }
        let reply = req.send().promise.await?;
        Ok(reply.get()?.get_written())
    }

    /// Persist any buffered writes. Idempotent.
    pub async fn flush(&self) -> Result<(), NotAvailable> {
        let req = self.inner.flush_request();
        req.send().promise.await?;
        Ok(())
    }

    /// Stat this open handle — cheaper than [`FsClient::stat`] when you already have it.
    pub async fn metadata(&self) -> Result<Metadata, NotAvailable> {
        let req = self.inner.metadata_request();
        let reply = req.send().promise.await?;
        Metadata::from_reader(reply.get()?.get_metadata()?)
    }
}

/// Re-export the raw generated module for power users (server impls, tests).
pub mod raw {
    pub use super::fs_capnp::*;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn clear_qos_env() {
        for (key, _) in env::vars() {
            if key.starts_with("QOS_") {
                unsafe { env::remove_var(&key) };
            }
        }
    }

    #[test]
    fn try_connect_without_env_returns_service_fd_missing() {
        clear_qos_env();
        match try_connect() {
            Err(NotAvailable::ServiceFdMissing) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
            Ok(_) => panic!("expected ServiceFdMissing"),
        }
    }

    #[test]
    fn open_mode_maps_to_wire_enum() {
        // Round-trip the enum. If someone reorders the schema the
        // From impl won't compile — this is belt-and-suspenders.
        assert!(matches!(
            fs_capnp::OpenMode::from(OpenMode::Read),
            fs_capnp::OpenMode::Read
        ));
        assert!(matches!(
            fs_capnp::OpenMode::from(OpenMode::Write),
            fs_capnp::OpenMode::Write
        ));
        assert!(matches!(
            fs_capnp::OpenMode::from(OpenMode::Append),
            fs_capnp::OpenMode::Append
        ));
    }
}
