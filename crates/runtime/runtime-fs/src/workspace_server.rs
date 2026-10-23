//! Serving the workspace filesystem protocol from a host's own workspace,
//! a client of [`crate::FsClient`] browses, stats and uses explicit file handles.
//!
//! The host supplies a [`WorkspaceBackend`]; its calls may block (reading a
//! file can hydrate it from a peer), so they run on the host's runtime,
//! never on the connection's. Legacy path backends remain read only; explicit
//! file capabilities may implement writes. `mkdir` and `remove` remain refused.

use std::{
    io,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use capnp::capability::Promise;
use tokio::sync::oneshot;

use crate::fs_capnp::{self, file_handle, workspace_fs};
use crate::{DirEntry, EntryKind, MAX_READ_CHUNK, Metadata, OpenMode};

const MAX_BACKEND_CALLS: usize = 8;

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
            let local = tokio::task::LocalSet::new();
            local.block_on(&local_runtime, async move {
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
/// Dropping the owner closes the RPC connection and joins its owned thread.
/// Backend calls already running are not forcibly interrupted: providers must
/// bound their calls and retire their native authority before releasing custody.
pub struct ConnectedWorkspaceServer {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<io::Result<()>>>,
}
impl ConnectedWorkspaceServer {
    /// Wait for the actual RPC connection to finish without requesting stop.
    pub fn wait(mut self) -> io::Result<()> {
        self.join()
    }
    /// Request stop and report the actual owned worker departure.
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
            let local = tokio::task::LocalSet::new();
            local.block_on(&local_runtime, async move {
                let server = Rc::new(Server {
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
        let server = Rc::new(Server {
            backend: Arc::clone(backend),
            runtime: runtime.clone(),
            calls: Arc::new(tokio::sync::Semaphore::new(MAX_BACKEND_CALLS)),
            handoffs: None,
            drain: None,
        });
        tokio::task::spawn_local(async move {
            if let Err(error) = serve_connection(stream, server).await {
                eprintln!("workspace file connection closed: {error}");
            }
        });
    }
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc servers are Rc-based and run on a LocalSet by construction"
)]
async fn serve_connection(stream: tokio::net::UnixStream, server: Rc<Server>) -> io::Result<()> {
    let client: workspace_fs::Client = capnp_rpc::new_client_from_rc(server);
    runtime_portal::serve_connected_io(stream, client.client)
        .await
        .map_err(|error| io::Error::other(error.to_string()))
}

#[path = "workspace_server/directory.rs"]
mod directory;

struct Server {
    backend: Arc<dyn WorkspaceBackend>,
    runtime: tokio::runtime::Handle,
    calls: Arc<tokio::sync::Semaphore>,
    handoffs: Option<Arc<PendingHandoffRegistry>>,
    drain: Option<Arc<pending_handoff::CallDrain>>,
}

impl Server {
    // Fail busy without queuing application-owned payload buffers or callbacks.
    fn admit_call(&self) -> Result<tokio::sync::OwnedSemaphorePermit, capnp::Error> {
        Arc::clone(&self.calls)
            .try_acquire_owned()
            .map_err(|_| capnp::Error::failed("workspace backend busy".into()))
    }
    /// Runs one blocking backend call on the host's runtime.
    async fn call<T: Send + 'static>(
        &self,
        call: impl FnOnce(&dyn WorkspaceBackend) -> Result<T, String> + Send + 'static,
    ) -> Result<T, capnp::Error> {
        let backend = Arc::clone(&self.backend);
        let permit = self.admit_call()?;
        self.runtime
            .spawn_blocking(move || {
                let _permit = permit;
                call(backend.as_ref())
            })
            .await
            .map_err(|error| capnp::Error::failed(format!("workspace backend failed: {error}")))?
            .map_err(capnp::Error::failed)
    }
    async fn call_file<T: Send + 'static>(
        &self,
        file: Arc<dyn WorkspaceFileBackend>,
        call: impl FnOnce(&dyn WorkspaceFileBackend) -> Result<T, String> + Send + 'static,
    ) -> Result<Option<T>, capnp::Error> {
        let permit = self.admit_call()?;
        self.call_file_admitted(file, permit, call).await
    }
    async fn call_file_admitted<T: Send + 'static>(
        &self,
        file: Arc<dyn WorkspaceFileBackend>,
        permit: tokio::sync::OwnedSemaphorePermit,
        call: impl FnOnce(&dyn WorkspaceFileBackend) -> Result<T, String> + Send + 'static,
    ) -> Result<Option<T>, capnp::Error> {
        self.runtime
            .spawn_blocking(move || {
                let _permit = permit;
                if let Some(scope) = file.live_scope() {
                    scope?;
                    let result = call(file.as_ref());
                    file.live_scope().ok_or("live scope disappeared")??;
                    Ok(result.ok())
                } else {
                    call(file.as_ref()).map(Some)
                }
            })
            .await
            .map_err(|error| {
                capnp::Error::failed(format!("workspace file backend failed: {error}"))
            })?
            .map_err(capnp::Error::failed)
    }
}

fn path_param(value: capnp::Result<capnp::text::Reader<'_>>) -> Result<String, capnp::Error> {
    value?
        .to_str()
        .map(|path| path.trim_matches('/').to_owned())
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

fn read_only() -> capnp::Error {
    capnp::Error::failed("this workspace is served read only".into())
}

fn write_metadata(mut out: fs_capnp::metadata::Builder<'_>, metadata: &Metadata) {
    out.set_kind(match metadata.kind {
        EntryKind::File => fs_capnp::EntryKind::File,
        EntryKind::Directory => fs_capnp::EntryKind::Directory,
        EntryKind::Symlink => fs_capnp::EntryKind::Symlink,
        EntryKind::Other => fs_capnp::EntryKind::Other,
    });
    out.set_size(metadata.size);
    out.set_modified_unix_seconds(metadata.modified_unix_seconds.unwrap_or(0));
    out.set_needs_hydration(metadata.needs_hydration);
}

#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl workspace_fs::Server for Server {
    fn open(
        self: Rc<Self>,
        params: workspace_fs::OpenParams,
        mut results: workspace_fs::OpenResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let request = params.get()?;
            let mode = match request.get_mode()? {
                fs_capnp::OpenMode::Read => OpenMode::Read,
                fs_capnp::OpenMode::Write => OpenMode::Write,
                fs_capnp::OpenMode::Append => OpenMode::Append,
                fs_capnp::OpenMode::ReadWriteExisting => OpenMode::ReadWriteExisting,
                fs_capnp::OpenMode::WriteExisting => OpenMode::WriteExisting,
            };
            let path = path_param(request.get_path())?;
            let selected = path.clone();
            let opened = self
                .call(move |backend| backend.open(&selected, mode))
                .await?;
            let source = if let Some(file) = opened {
                let metadata = self
                    .call_file(Arc::clone(&file), WorkspaceFileBackend::metadata)
                    .await?;
                let metadata = metadata
                    .ok_or_else(|| capnp::Error::failed("native open metadata refused".into()))?;
                if metadata.kind != EntryKind::File {
                    return Err(capnp::Error::failed(format!("{path} is not a file")));
                }
                FileSource::Capability(file)
            } else {
                if mode != OpenMode::Read {
                    return Err(read_only());
                }
                let checked = path.clone();
                let metadata = self.call(move |backend| backend.stat(&checked)).await?;
                if metadata.kind != EntryKind::File {
                    return Err(capnp::Error::failed(format!("{path} is not a file")));
                }
                FileSource::Path(path)
            };
            let handle: file_handle::Client = capnp_rpc::new_client(File {
                server: self,
                source,
                mode,
            });
            results.get().set_handle(handle);
            Ok(())
        })
    }

    fn list(
        self: Rc<Self>,
        params: workspace_fs::ListParams,
        mut results: workspace_fs::ListResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let path = path_param(params.get()?.get_path())?;
            let entries = self.call(move |backend| backend.list(&path)).await?;
            let count = u32::try_from(entries.len())
                .map_err(|_| capnp::Error::failed("too many entries".into()))?;
            let mut out = results.get().init_entries(count);
            for (index, entry) in (0..count).zip(&entries) {
                let mut row = out.reborrow().get(index);
                row.set_name(&entry.name);
                write_metadata(row.init_metadata(), &entry.metadata);
            }
            Ok(())
        })
    }

    fn stat(
        self: Rc<Self>,
        params: workspace_fs::StatParams,
        mut results: workspace_fs::StatResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let path = path_param(params.get()?.get_path())?;
            let metadata = self.call(move |backend| backend.stat(&path)).await?;
            write_metadata(results.get().init_metadata(), &metadata);
            Ok(())
        })
    }

    fn directory(
        self: Rc<Self>,
        _: workspace_fs::DirectoryParams,
        mut results: workspace_fs::DirectoryResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
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
            results
                .get()
                .set_handle(directory::client(Rc::clone(&self), backend));
            Ok(())
        })
    }

    fn mkdir(
        self: Rc<Self>,
        _: workspace_fs::MkdirParams,
        _: workspace_fs::MkdirResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(read_only())
    }

    fn remove(
        self: Rc<Self>,
        _: workspace_fs::RemoveParams,
        _: workspace_fs::RemoveResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(read_only())
    }
}

enum FileSource {
    Path(String),
    Capability(Arc<dyn WorkspaceFileBackend>),
}
struct File {
    server: Rc<Server>,
    source: FileSource,
    mode: OpenMode,
}

impl File {
    fn refuse_operation(self: &Rc<Self>) -> Promise<(), capnp::Error> {
        let this = Rc::clone(self);
        Promise::from_future(async move {
            let FileSource::Capability(file) = &this.source else {
                return Err(read_only());
            };
            // Only actual native providers with successful before/after scope checks
            // can distinguish an unsupported per-file operation from authority loss.
            this.server
                .call_file(Arc::clone(file), |_| {
                    Err::<(), _>("unsupported file operation".into())
                })
                .await?;
            Ok(())
        })
    }
}

#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl file_handle::Server for File {
    fn check_scope(
        self: Rc<Self>,
        _: file_handle::CheckScopeParams,
        _: file_handle::CheckScopeResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let FileSource::Capability(file) = &self.source else {
                return Err(read_only());
            };
            let live = self
                .server
                .call_file(Arc::clone(file), |file| {
                    file.live_scope().ok_or("scope check unavailable")?
                })
                .await?;
            live.ok_or_else(|| capnp::Error::failed("live scope unavailable".into()))
        })
    }
    fn read(
        self: Rc<Self>,
        params: file_handle::ReadParams,
        mut results: file_handle::ReadResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let request = params.get()?;
            let (offset, max_len) = (request.get_offset(), request.get_max_len());
            if !matches!(self.mode, OpenMode::Read | OpenMode::ReadWriteExisting) {
                self.refuse_operation().await?;
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
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
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            if u32::try_from(bytes.len())
                .ok()
                .is_none_or(|length| length > limit)
            {
                return Err(capnp::Error::failed(
                    "file backend exceeded read bound".into(),
                ));
            }
            results.get().set_bytes(&bytes);
            Ok(())
        })
    }

    fn write(
        self: Rc<Self>,
        params: file_handle::WriteParams,
        mut results: file_handle::WriteResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            if !matches!(
                self.mode,
                OpenMode::Write | OpenMode::WriteExisting | OpenMode::ReadWriteExisting
            ) {
                self.refuse_operation().await?;
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            }
            let FileSource::Capability(file) = &self.source else {
                return Err(read_only());
            };
            let request = params.get()?;
            let data = request.get_bytes()?;
            let length = u32::try_from(data.len())
                .map_err(|_| capnp::Error::failed("write exceeds byte bound".into()))?;
            if length > MAX_READ_CHUNK {
                return Err(capnp::Error::failed("write exceeds byte bound".into()));
            }
            // Finite admission precedes allocation: full pipelines receive Busy
            // immediately rather than retaining another copied payload buffer.
            let written = {
                let permit = self.server.admit_call()?;
                let bytes = data.to_vec();
                let offset = request.get_offset();
                self.server
                    .call_file_admitted(Arc::clone(file), permit, move |file| {
                        file.write(offset, &bytes)
                    })
                    .await?
            };
            let Some(written) = written else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            if written > length {
                return Err(capnp::Error::failed("invalid write receipt".into()));
            }
            results.get().set_written(written);
            Ok(())
        })
    }

    fn append(
        self: Rc<Self>,
        params: file_handle::AppendParams,
        mut results: file_handle::AppendResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            if self.mode != OpenMode::Append {
                self.refuse_operation().await?;
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            }
            let FileSource::Capability(file) = &self.source else {
                return Err(read_only());
            };
            let request = params.get()?;
            let data = request.get_bytes()?;
            let length = u32::try_from(data.len())
                .map_err(|_| capnp::Error::failed("append exceeds byte bound".into()))?;
            if length > crate::MAX_WRITE_CHUNK {
                return Err(capnp::Error::failed("append exceeds byte bound".into()));
            }
            let written = {
                let permit = self.server.admit_call()?;
                let bytes = data.to_vec();
                self.server
                    .call_file_admitted(Arc::clone(file), permit, move |file| file.append(&bytes))
                    .await?
            };
            let Some(written) = written else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            if written > length {
                return Err(capnp::Error::failed("invalid append count".into()));
            }
            results.get().set_written(written);
            Ok(())
        })
    }

    fn flush(
        self: Rc<Self>,
        _: file_handle::FlushParams,
        mut results: file_handle::FlushResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            match &self.source {
                FileSource::Path(_) => Ok(()),
                FileSource::Capability(file) => {
                    if self
                        .server
                        .call_file(Arc::clone(file), WorkspaceFileBackend::flush)
                        .await?
                        .is_none()
                    {
                        results
                            .get()
                            .set_status(fs_capnp::NativeEntryStatus::Refused);
                    }
                    Ok(())
                }
            }
        })
    }

    fn metadata(
        self: Rc<Self>,
        _: file_handle::MetadataParams,
        mut results: file_handle::MetadataResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
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
            let Some(metadata) = metadata else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            write_metadata(results.get().init_metadata(), &metadata);
            Ok(())
        })
    }
}

#[path = "workspace_server/pending_handoff.rs"]
mod pending_handoff;
pub use pending_handoff::{
    PendingHandoffRegistry, serve_history_read_connected, serve_pending_connected,
};
