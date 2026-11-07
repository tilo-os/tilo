//! Serving the workspace filesystem protocol from a host's own workspace,
//! a client of [`tilo_fs_client::FsClient`] browses, stats and uses explicit
//! file handles.
//!
//! The host supplies a [`WorkspaceBackend`]; its calls may block (reading a
//! file can hydrate it from a peer), so they run on the host's runtime,
//! never on the connection's. Legacy path backends remain read only; explicit
//! file capabilities may implement writes. `mkdir` and `remove` remain refused.

use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use tilo_fs_client::connection::{Capabilities, ServeObject, serve_connected};
use tilo_fs_client::wire::{FsReply, FsRequest, NativeEntryStatus};
use tokio::sync::oneshot;

use crate::{DirEntry, EntryKind, MAX_READ_CHUNK, Metadata, OpenMode};

pub(crate) const MAX_BACKEND_CALLS: usize = 8;

/// An already-admitted live file capability supplied by the authenticated host.
/// Methods must enforce their own current authority and bounded I/O deadlines.
/// A handle never resolves an ambient path or grants access to its parent.
pub trait WorkspaceFileBackend: Send + Sync + 'static {
    /// Optional authentic live-scope check; default preserves legacy RPC failures.
    /// Typed native providers return Some for both success and revocation.
    fn live_scope(&self) -> Option<Result<(), String>> {
        None
    }
    /// Actual held native identity for directory opens; legacy backends expose none.
    /// # Errors
    /// Refuses unavailable authority or failed descriptor identity lookup.
    fn native_identity(&self) -> Result<Option<[u64; 2]>, String> {
        Ok(None)
    }
    /// # Errors
    /// Refuses unavailable authority or a failed bounded read.
    fn read(&self, offset: u64, max_len: u32) -> Result<Vec<u8>, String>;
    /// # Errors
    /// Refuses unavailable write authority or a failed write.
    fn write(&self, offset: u64, bytes: &[u8]) -> Result<u32, String>;

    /// One non-idempotent atomic append. Unsupported by default.
    /// # Errors
    /// Refuses unsupported append, unavailable authority or a failed write.
    fn append(&self, _bytes: &[u8]) -> Result<u32, String> {
        Err("atomic append unsupported".into())
    }
    /// # Errors
    /// Refuses unavailable authority or failed persistence.
    fn flush(&self) -> Result<(), String>;
    /// # Errors
    /// Refuses unavailable authority or failed descriptor metadata.
    fn metadata(&self) -> Result<Metadata, String>;
}

/// A workspace a host serves. Paths are workspace-relative with `/`
/// separators; the root is `""`.
pub trait WorkspaceBackend: Send + Sync + 'static {
    /// Independently admitted native directory custody. Selected files return None.
    /// # Errors
    /// Refuses unavailable authority or failed directory admission.
    fn directory(
        &self,
    ) -> Result<Option<Arc<dyn crate::directory_backend::WorkspaceDirectoryBackend>>, String> {
        Ok(None)
    }

    /// Optionally supply an already-admitted file capability. Returning None
    /// selects the original path-backed behavior, which permits Read only.
    /// Write retains its canonical truncate/create meaning; Append retains
    /// append semantics. A provider must refuse modes it cannot implement.
    /// # Errors
    /// Refuses unavailable authority, changed selection or unsupported modes.
    fn open(
        &self,
        _path: &str,
        _mode: OpenMode,
    ) -> Result<Option<Arc<dyn WorkspaceFileBackend>>, String> {
        Ok(None)
    }

    /// The entries of the directory at `path`.
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, String>;

    fn stat(&self, path: &str) -> Result<Metadata, String>;

    /// Up to `max_len` bytes of the file at `path` from `offset`, empty at
    /// its end.
    ///
    /// # Errors
    /// `path` is not a file here, or its content cannot be read.
    fn read(&self, path: &str, offset: u64, max_len: u32) -> Result<Vec<u8>, String>;
}

/// A running server; stops, and removes its socket, when dropped.
pub struct WorkspaceServer {
    socket: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for WorkspaceServer {
    /// Closes every connection and releases the backend.
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Serves `backend` at `socket`, owner-only, to every connection, from a
/// thread of its own, running the backend's calls on `runtime`. A stale
/// socket file is replaced.
///
/// # Errors
/// `socket` is not a socket path, or cannot be bound, or the thread cannot
/// start.
pub fn serve_workspace(
    socket: &Path,
    backend: Arc<dyn WorkspaceBackend>,
    runtime: tokio::runtime::Handle,
) -> io::Result<WorkspaceServer> {
    let listener = tilo_files::native::private::bind_socket(socket)?;
    listener.set_nonblocking(true)?;
    let (stop, stopped) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("workspace-fs".into())
        .spawn(move || {
            let local_runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("workspace file server could not start: {error}");
                    return;
                }
            };
            local_runtime.block_on(async move {
                let listener = match tokio::net::UnixListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        eprintln!("workspace file socket unusable: {error}");
                        return;
                    }
                };
                tokio::select! {
                    () = accept(&listener, &backend, &runtime) => {}
                    _ = stopped => {}
                }
            });
        })?;
    Ok(WorkspaceServer {
        socket: socket.to_path_buf(),
        stop: Some(stop),
        thread: Some(thread),
    })
}

/// Exactly one authenticated private connected stream, with no discovery name.
/// Dropping the owner closes the connection and joins its owned thread.
/// Backend calls already running are not forcibly interrupted: providers must
/// bound their calls and retire their native authority before releasing custody.
pub struct ConnectedWorkspaceServer {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<io::Result<()>>>,
}

impl ConnectedWorkspaceServer {
    /// Wait for the actual connection to finish without requesting stop.
    /// # Errors
    /// Returns the connection's own failure, or a panic in its worker.
    pub fn wait(mut self) -> io::Result<()> {
        self.join()
    }
    /// Ask it to stop and report the actual owned worker departure.
    /// # Errors
    /// Returns the connection's own failure, or a panic in its worker.
    pub fn retire(mut self) -> io::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.join()
    }
    fn join(&mut self) -> io::Result<()> {
        let thread = self
            .thread
            .take()
            .ok_or_else(|| io::Error::other("workspace worker already joined"))?;
        thread.join().map_err(|panic| {
            let message = panic
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| panic.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            io::Error::other(format!("workspace file worker panicked: {message}"))
        })?
    }
}

impl Drop for ConnectedWorkspaceServer {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Serve an attested, owned stream through the existing `WorkspaceFs` protocol.
/// Transport authentication and source authority remain the caller's boundary;
/// this function opens no socket path and accepts no credential claims.
/// # Errors
/// Refuses unusable streams or failure to create the owned server runtime/thread.
pub fn serve_workspace_connected(
    stream: std::os::unix::net::UnixStream,
    backend: Arc<dyn WorkspaceBackend>,
    runtime: tokio::runtime::Handle,
) -> io::Result<ConnectedWorkspaceServer> {
    serve_workspace_connected_with_handoffs(stream, backend, runtime, None)
}

/// Same owned source connection, with an optional source-resident pending-only
/// handoff registry. Registry contents arise only from actual Pending servers.
/// # Errors
/// Refuses invalid streams or unavailable owned server resources.
pub fn serve_workspace_connected_with_handoffs(
    stream: std::os::unix::net::UnixStream,
    backend: Arc<dyn WorkspaceBackend>,
    runtime: tokio::runtime::Handle,
    handoffs: Option<Arc<PendingHandoffRegistry>>,
) -> io::Result<ConnectedWorkspaceServer> {
    stream.set_nonblocking(true)?;
    let local_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let stream = {
        let _entered = local_runtime.enter();
        tokio::net::UnixStream::from_std(stream)?
    };
    let (stop, stopped) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("workspace-file-capability".into())
        .spawn(move || {
            local_runtime.block_on(async move {
                let server = Arc::new(Server {
                    backend,
                    runtime,
                    calls: Arc::new(tokio::sync::Semaphore::new(MAX_BACKEND_CALLS)),
                    handoffs,
                    drain: None,
                });
                tokio::select! {
                    result = serve_connection(stream, server) => result,
                    _ = stopped => Ok(()),
                }
            })
        })?;
    Ok(ConnectedWorkspaceServer {
        stop: Some(stop),
        thread: Some(thread),
    })
}

async fn accept(
    listener: &tokio::net::UnixListener,
    backend: &Arc<dyn WorkspaceBackend>,
    runtime: &tokio::runtime::Handle,
) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                eprintln!("workspace file accept failed: {error}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let server = Arc::new(Server {
            backend: Arc::clone(backend),
            runtime: runtime.clone(),
            calls: Arc::new(tokio::sync::Semaphore::new(MAX_BACKEND_CALLS)),
            handoffs: None,
            drain: None,
        });
        tokio::spawn(async move {
            if let Err(error) = serve_connection(stream, server).await {
                eprintln!("workspace file connection closed: {error}");
            }
        });
    }
}

async fn serve_connection(stream: tokio::net::UnixStream, server: Arc<Server>) -> io::Result<()> {
    serve_connected(stream, server).await
}

#[path = "workspace_server/directory.rs"]
mod directory;

pub(crate) struct Server {
    pub(crate) backend: Arc<dyn WorkspaceBackend>,
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) calls: Arc<tokio::sync::Semaphore>,
    pub(crate) handoffs: Option<Arc<PendingHandoffRegistry>>,
    pub(crate) drain: Option<Arc<pending_handoff::CallDrain>>,
}

impl Server {
    // Fail busy without queuing application-owned payload buffers or callbacks.
    pub(crate) fn admit_call(&self) -> Result<tokio::sync::OwnedSemaphorePermit, String> {
        Arc::clone(&self.calls)
            .try_acquire_owned()
            .map_err(|_| "workspace backend busy".to_string())
    }

    /// Runs one blocking backend call on the host's runtime.
    async fn blocking<T: Send + 'static>(
        &self,
        permit: tokio::sync::OwnedSemaphorePermit,
        call: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        self.runtime
            .spawn_blocking(move || {
                let _permit = permit;
                call()
            })
            .await
            .map_err(|error| format!("workspace backend failed: {error}"))?
    }

    pub(crate) async fn call<T: Send + 'static>(
        &self,
        call: impl FnOnce(&dyn WorkspaceBackend) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let permit = self.admit_call()?;
        let backend = Arc::clone(&self.backend);
        self.blocking(permit, move || call(backend.as_ref())).await
    }

    pub(crate) async fn call_file<T: Send + 'static>(
        &self,
        file: Arc<dyn WorkspaceFileBackend>,
        call: impl FnOnce(&dyn WorkspaceFileBackend) -> Result<T, String> + Send + 'static,
    ) -> Result<Option<T>, String> {
        let permit = self.admit_call()?;
        self.call_file_admitted(file, permit, call).await
    }

    pub(crate) async fn call_file_admitted<T: Send + 'static>(
        &self,
        file: Arc<dyn WorkspaceFileBackend>,
        permit: tokio::sync::OwnedSemaphorePermit,
        call: impl FnOnce(&dyn WorkspaceFileBackend) -> Result<T, String> + Send + 'static,
    ) -> Result<Option<T>, String> {
        self.blocking(permit, move || {
            checked(|| file.live_scope(), || call(file.as_ref()))
        })
        .await
    }

    /// Runs a directory backend call with its drain entry and fresh scope
    /// brackets, so a handoff waits for it and a revocation reads as a refusal.
    pub(crate) async fn run_directory<T: Send + 'static>(
        &self,
        backend: Arc<dyn crate::directory_backend::WorkspaceDirectoryBackend>,
        call: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<Option<T>, String> {
        let permit = self.admit_call()?;
        let drain = match &self.drain {
            Some(drain) => Some(drain.enter()?),
            None => None,
        };
        self.blocking(permit, move || {
            let _drain = drain;
            backend.check()?;
            let value = call();
            backend.check()?;
            Ok(value.ok())
        })
        .await
    }

    /// Runs one directory call behind its drain entry, without scope brackets.
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        call: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let permit = self.admit_call()?;
        let drain = match &self.drain {
            Some(drain) => Some(drain.enter()?),
            None => None,
        };
        self.blocking(permit, move || {
            let _drain = drain;
            call()
        })
        .await
    }
}

/// Brackets `call` with the object's own live-scope checks where it has them.
pub(crate) fn checked<T>(
    scope: impl Fn() -> Option<Result<(), String>>,
    call: impl FnOnce() -> Result<T, String>,
) -> Result<Option<T>, String> {
    if let Some(live) = scope() {
        live?;
        let value = call();
        scope().ok_or("live scope disappeared")??;
        Ok(value.ok())
    } else {
        call().map(Some)
    }
}

fn path_param(path: &str) -> String {
    path.trim_matches('/').to_owned()
}

pub(crate) fn ack(ok: bool) -> NativeEntryStatus {
    if ok {
        NativeEntryStatus::Ok
    } else {
        NativeEntryStatus::Refused
    }
}

pub(crate) fn read_only() -> String {
    "this workspace is served read only".into()
}

impl ServeObject for Server {
    fn dispatch(
        self: Arc<Self>,
        capabilities: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        let capabilities = capabilities.clone();
        Box::pin(async move {
            match request {
                FsRequest::Open { path, mode } => {
                    let selected = path_param(&path);
                    let opened = {
                        let selected = selected.clone();
                        self.call(move |backend| backend.open(&selected, mode))
                            .await?
                    };
                    let source = if let Some(file) = opened {
                        let metadata = self
                            .call_file(Arc::clone(&file), WorkspaceFileBackend::metadata)
                            .await?
                            .ok_or_else(|| "native open metadata refused".to_string())?;
                        if metadata.kind != EntryKind::File {
                            return Err(format!("{selected} is not a file"));
                        }
                        FileSource::Capability(file)
                    } else {
                        if mode != OpenMode::Read {
                            return Err(read_only());
                        }
                        let checked_path = selected.clone();
                        let metadata = self
                            .call(move |backend| backend.stat(&checked_path))
                            .await?;
                        if metadata.kind != EntryKind::File {
                            return Err(format!("{selected} is not a file"));
                        }
                        FileSource::Path(selected)
                    };
                    let handle = capabilities.register(Arc::new(File {
                        server: Arc::clone(&self),
                        source,
                        mode,
                    }));
                    Ok(FsReply::Handle(Some(handle)))
                }
                FsRequest::List { path } => {
                    let path = path_param(&path);
                    let entries = self.call(move |backend| backend.list(&path)).await?;
                    Ok(FsReply::Entries(Some(entries)))
                }
                FsRequest::Stat { path } => {
                    let path = path_param(&path);
                    let metadata = self.call(move |backend| backend.stat(&path)).await?;
                    Ok(FsReply::Metadata(Some(metadata)))
                }
                FsRequest::Directory => {
                    let backend = self
                        .call(|backend| {
                            let directory = backend.directory()?;
                            if let Some(directory) = &directory {
                                directory.check()?;
                            }
                            Ok(directory)
                        })
                        .await?
                        .ok_or_else(read_only)?;
                    let handle = capabilities.register(Arc::new(directory::Directory {
                        server: Arc::clone(&self),
                        backend,
                    }));
                    Ok(FsReply::Handle(Some(handle)))
                }
                FsRequest::Mkdir { .. } | FsRequest::Remove { .. } => Err(read_only()),
                _ => Err("unexpected workspace request".into()),
            }
        })
    }
}

pub(crate) enum FileSource {
    Path(String),
    Capability(Arc<dyn WorkspaceFileBackend>),
}

pub(crate) struct File {
    pub(crate) server: Arc<Server>,
    pub(crate) source: FileSource,
    pub(crate) mode: OpenMode,
}

impl File {
    async fn refuse_operation(&self) -> Result<(), String> {
        let FileSource::Capability(file) = &self.source else {
            return Err(read_only());
        };
        // Only actual native providers with successful before/after scope checks
        // can distinguish an unsupported per-file operation from authority loss.
        self.server
            .call_file(Arc::clone(file), |_| {
                Err::<(), _>("unsupported file operation".into())
            })
            .await?;
        Ok(())
    }
}

impl ServeObject for File {
    #[allow(
        clippy::too_many_lines,
        clippy::significant_drop_tightening,
        reason = "one request table per object; the call admission is passed to the call it admits"
    )]
    fn dispatch(
        self: Arc<Self>,
        _: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        Box::pin(async move {
            match request {
                FsRequest::CheckScope => {
                    let FileSource::Capability(file) = &self.source else {
                        return Err(read_only());
                    };
                    let live = self
                        .server
                        .call_file(Arc::clone(file), |file| {
                            file.live_scope().ok_or("scope check unavailable")?
                        })
                        .await?;
                    live.ok_or_else(|| "live scope unavailable".to_string())?;
                    Ok(FsReply::Empty)
                }
                FsRequest::Read { offset, max_len } => {
                    if !matches!(self.mode, OpenMode::Read | OpenMode::ReadWriteExisting) {
                        self.refuse_operation().await?;
                        return Ok(FsReply::Bytes(None));
                    }
                    let limit = max_len.min(MAX_READ_CHUNK);
                    let bytes = match &self.source {
                        FileSource::Path(path) => {
                            let path = path.clone();
                            self.server
                                .call(move |backend| backend.read(&path, offset, limit))
                                .await
                                .map(Some)?
                        }
                        FileSource::Capability(file) => {
                            self.server
                                .call_file(Arc::clone(file), move |file| file.read(offset, limit))
                                .await?
                        }
                    };
                    let Some(bytes) = bytes else {
                        return Ok(FsReply::Bytes(None));
                    };
                    if u32::try_from(bytes.len())
                        .ok()
                        .is_none_or(|length| length > limit)
                    {
                        return Err("file backend exceeded read bound".into());
                    }
                    Ok(FsReply::Bytes(Some(bytes)))
                }
                FsRequest::Write { offset, bytes } => {
                    if !matches!(
                        self.mode,
                        OpenMode::Write | OpenMode::WriteExisting | OpenMode::ReadWriteExisting
                    ) {
                        self.refuse_operation().await?;
                        return Ok(FsReply::Written(None));
                    }
                    let FileSource::Capability(file) = &self.source else {
                        return Err(read_only());
                    };
                    let length = u32::try_from(bytes.len())
                        .map_err(|_| "write exceeds byte bound".to_string())?;
                    if length > MAX_READ_CHUNK {
                        return Err("write exceeds byte bound".into());
                    }
                    // Finite admission precedes allocation: full pipelines receive Busy
                    // immediately rather than retaining another copied payload buffer.
                    let permit = self.server.admit_call()?;
                    let written = self
                        .server
                        .call_file_admitted(Arc::clone(file), permit, move |file| {
                            file.write(offset, &bytes)
                        })
                        .await?;
                    let Some(written) = written else {
                        return Ok(FsReply::Written(None));
                    };
                    if written > length {
                        return Err("invalid write receipt".into());
                    }
                    Ok(FsReply::Written(Some(written)))
                }
                FsRequest::Append { bytes } => {
                    if self.mode != OpenMode::Append {
                        self.refuse_operation().await?;
                        return Ok(FsReply::Written(None));
                    }
                    let FileSource::Capability(file) = &self.source else {
                        return Err(read_only());
                    };
                    let length = u32::try_from(bytes.len())
                        .map_err(|_| "append exceeds byte bound".to_string())?;
                    if length > crate::MAX_WRITE_CHUNK {
                        return Err("append exceeds byte bound".into());
                    }
                    let permit = self.server.admit_call()?;
                    let written = self
                        .server
                        .call_file_admitted(Arc::clone(file), permit, move |file| {
                            file.append(&bytes)
                        })
                        .await?;
                    let Some(written) = written else {
                        return Ok(FsReply::Written(None));
                    };
                    if written > length {
                        return Err("invalid append count".into());
                    }
                    Ok(FsReply::Written(Some(written)))
                }
                FsRequest::Flush => match &self.source {
                    FileSource::Path(_) => Ok(FsReply::Ack(NativeEntryStatus::Ok)),
                    FileSource::Capability(file) => {
                        let flushed = self
                            .server
                            .call_file(Arc::clone(file), WorkspaceFileBackend::flush)
                            .await?
                            .is_some();
                        Ok(FsReply::Ack(if flushed {
                            NativeEntryStatus::Ok
                        } else {
                            NativeEntryStatus::Refused
                        }))
                    }
                },
                FsRequest::HandleMetadata => {
                    let metadata = match &self.source {
                        FileSource::Path(path) => {
                            let path = path.clone();
                            self.server
                                .call(move |backend| backend.stat(&path))
                                .await
                                .map(Some)?
                        }
                        FileSource::Capability(file) => {
                            self.server
                                .call_file(Arc::clone(file), WorkspaceFileBackend::metadata)
                                .await?
                        }
                    };
                    Ok(FsReply::Metadata(metadata))
                }
                _ => Err("unexpected file request".into()),
            }
        })
    }
}

#[path = "workspace_server/pending_handoff.rs"]
mod pending_handoff;
pub use pending_handoff::{
    PendingHandoffRegistry, serve_history_read_connected, serve_pending_connected,
};
