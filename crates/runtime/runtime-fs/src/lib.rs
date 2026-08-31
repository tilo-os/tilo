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

// Every `async fn` here awaits a capnp-rpc promise, and capnp-rpc's promises are
// `!Send` by construction: a capability is reference-counted with `Rc` and its
// hooks are `dyn` traits without a `Send` bound, so the RPC system runs on one
// thread and is driven by `tokio::task::spawn_local`. There is no version of
// this client whose futures cross threads, which is why the module docs tell
// callers to run it inside a `LocalSet`. `future_not_send` has nothing to warn
// about here — it would only ever be silenced, never satisfied.
#![allow(clippy::future_not_send)]

use std::rc::Rc;

use runtime_portal::{NotAvailable, UnixCapnpClient, service_fd};

// Generated schema. `include!` at the crate root under the exact
// module name the capnp compiler emits (`fs_capnp` for schema/fs.capnp).
//
// capnpc rewrites this file on every build, so nothing in it can be fixed —
// the lint groups are switched off wholesale rather than chased. The same
// list guards `app_host_capnp` in `crates/packaging/app-host`.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    unused_imports,
    unused_qualifications,
    unused_results,
    non_camel_case_types,
    missing_docs,
    unused,
    dead_code,
    unsafe_code
)]
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
///
/// # Errors
///
/// [`NotAvailable::ServiceFdMissing`] when `QOS_SVC_FS_FD` is unset or names an fd that is
/// no longer open — the ordinary case off qOS, where the caller is expected to fall back to
/// a stock-Linux path rather than treat it as a fault. [`NotAvailable::ConnectionFailed`]
/// when the fd is there but cannot be lifted onto the tokio reactor.
///
/// Dialling is local: nothing is sent to the proxy, so a broken or absent server on the far
/// end shows up on the first call made through the returned client, not here.
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
    fn from_reader(r: fs_capnp::metadata::Reader<'_>) -> Self {
        let secs = r.get_modified_unix_seconds();
        // An unknown wire value collapses to `Other` rather than failing, since
        // schema evolution must not break older clients. Nothing else here can
        // fail, so this is infallible and does not need a `Result`.
        let kind = r.get_kind().map_or(EntryKind::Other, Into::into);
        Self {
            kind,
            size: r.get_size(),
            modified_unix_seconds: if secs == 0 { None } else { Some(secs) },
            needs_hydration: r.get_needs_hydration(),
        }
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
    ///
    /// # Errors
    ///
    /// [`NotAvailable::Capnp`] for everything the proxy can refuse: the path does not exist
    /// and `mode` is [`OpenMode::Read`], it resolves outside the workspace, the sandbox
    /// policy denies it, or it is a directory. The same variant covers a dropped connection,
    /// as `Disconnected` — a peer that went away is indistinguishable from one that refused,
    /// at this layer.
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
    ///
    /// # Errors
    ///
    /// [`NotAvailable::Capnp`] when the proxy refuses the path (missing, not a directory,
    /// outside the workspace, denied) or the connection has dropped, and also when an entry
    /// name is not valid UTF-8 — the whole listing fails rather than silently skipping the
    /// row, since a file the caller cannot be told about is worse than an error.
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
            let metadata = Metadata::from_reader(entry.get_metadata()?);
            out.push(DirEntry { name, metadata });
        }
        Ok(out)
    }

    /// Stat a single path; a missing path surfaces as [`NotAvailable::Capnp`].
    ///
    /// # Errors
    ///
    /// [`NotAvailable::Capnp`] when the path does not exist, resolves outside the workspace,
    /// is denied by the sandbox policy, or the connection has dropped. Decoding the reply
    /// cannot fail: an unrecognised entry kind is reported as [`EntryKind::Other`].
    pub async fn stat(&self, path: &str) -> Result<Metadata, NotAvailable> {
        let mut req = self.inner.stat_request();
        req.get().set_path(path);
        let reply = req.send().promise.await?;
        Ok(Metadata::from_reader(reply.get()?.get_metadata()?))
    }

    /// Create a directory (with intermediate mkdirs); idempotent if it already exists.
    ///
    /// # Errors
    ///
    /// [`NotAvailable::Capnp`] when a path component exists as a non-directory, the parent
    /// is not writable, the path resolves outside the workspace, or the connection has
    /// dropped. An already-existing directory is success, not an error.
    pub async fn mkdir(&self, path: &str) -> Result<(), NotAvailable> {
        let mut req = self.inner.mkdir_request();
        req.get().set_path(path);
        req.send().promise.await?;
        Ok(())
    }

    /// Remove a file (unlink) or an empty directory (rmdir).
    ///
    /// # Errors
    ///
    /// [`NotAvailable::Capnp`] when the path does not exist, names a directory that is not
    /// empty, is not writable or lies outside the workspace, or the connection has dropped.
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
    ///
    /// # Errors
    ///
    /// [`NotAvailable::Capnp`] when the handle was opened for writing, hydration of a
    /// remote-only file fails (the bytes may live on a peer that is unreachable — `open`
    /// succeeding does not mean the content is local), the underlying read fails, or the
    /// connection has dropped. Reading past the end is not an error; it yields no bytes.
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
    ///
    /// # Errors
    ///
    /// Whatever [`Self::read_at`] returns, on any chunk. A partial read is discarded rather
    /// than returned, so the caller never mistakes a truncated file for a whole one.
    pub async fn read_all(&self) -> Result<Vec<u8>, NotAvailable> {
        let mut buf = Vec::new();
        let mut offset: u64 = 0;
        loop {
            let chunk = self.read_at(offset, MAX_READ_CHUNK).await?;
            if chunk.is_empty() {
                return Ok(buf);
            }
            // Saturating rather than wrapping: reaching u64::MAX would take a
            // 16-exabyte file, but a wrap would silently restart the read at 0.
            offset = offset.saturating_add(chunk.len() as u64);
            buf.extend_from_slice(&chunk);
        }
    }

    /// Write `bytes` at `offset`; returns the bytes written.
    ///
    /// # Errors
    ///
    /// [`NotAvailable::Capnp`] when the handle was opened [`OpenMode::Read`], the workspace
    /// is out of space or over quota, the write fails, or the connection has dropped. A
    /// short write is reported in the return value, not as an error — check it.
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
    ///
    /// # Errors
    ///
    /// [`NotAvailable::Capnp`] when the underlying flush fails — which is where a write that
    /// was only buffered finally reports being out of space — or the connection has dropped.
    pub async fn flush(&self) -> Result<(), NotAvailable> {
        let req = self.inner.flush_request();
        req.send().promise.await?;
        Ok(())
    }

    /// Stat this open handle — cheaper than [`FsClient::stat`] when you already have it.
    ///
    /// # Errors
    ///
    /// [`NotAvailable::Capnp`] when the server has invalidated the handle (the file was
    /// removed under it) or the connection has dropped.
    pub async fn metadata(&self) -> Result<Metadata, NotAvailable> {
        let req = self.inner.metadata_request();
        let reply = req.send().promise.await?;
        Ok(Metadata::from_reader(reply.get()?.get_metadata()?))
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
