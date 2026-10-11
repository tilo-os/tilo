//! Serving the workspace filesystem protocol from a host's own workspace,
//! read only: a client of [`crate::FsClient`] browses, stats and reads it.
//!
//! The host supplies a [`WorkspaceBackend`]; its calls may block (reading a
//! file can hydrate it from a peer), so they run on the host's runtime,
//! never on the connection's. Writes, `mkdir` and `remove` are refused.

use std::{
    io,
    os::unix::fs::{FileTypeExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use capnp::capability::Promise;
use tokio::sync::oneshot;

use crate::fs_capnp::{self, file_handle, workspace_fs};
use crate::{DirEntry, EntryKind, MAX_READ_CHUNK, Metadata};

/// A workspace a host serves. Paths are workspace-relative with `/`
/// separators; the root is `""`.
pub trait WorkspaceBackend: Send + Sync + 'static {
    /// The entries of the directory at `path`.
    ///
    /// # Errors
    /// `path` is not a directory here.
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, String>;

    /// # Errors
    /// `path` is not in the workspace.
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
    match std::fs::symlink_metadata(socket) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(socket)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is not a socket", socket.display()),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = std::os::unix::net::UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
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

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc servers are Rc-based and run on a LocalSet by construction"
)]
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

struct Server {
    backend: Arc<dyn WorkspaceBackend>,
    runtime: tokio::runtime::Handle,
}

impl Server {
    /// Runs one blocking backend call on the host's runtime.
    async fn call<T: Send + 'static>(
        &self,
        call: impl FnOnce(&dyn WorkspaceBackend) -> Result<T, String> + Send + 'static,
    ) -> Result<T, capnp::Error> {
        let backend = Arc::clone(&self.backend);
        self.runtime
            .spawn_blocking(move || call(backend.as_ref()))
            .await
            .map_err(|error| capnp::Error::failed(format!("workspace backend failed: {error}")))?
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
            if request.get_mode()? != fs_capnp::OpenMode::Read {
                return Err(read_only());
            }
            let path = path_param(request.get_path())?;
            let checked = path.clone();
            let metadata = self.call(move |backend| backend.stat(&checked)).await?;
            if metadata.kind != EntryKind::File {
                return Err(capnp::Error::failed(format!("{path} is not a file")));
            }
            let handle: file_handle::Client = capnp_rpc::new_client(File { server: self, path });
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

struct File {
    server: Rc<Server>,
    path: String,
}

#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl file_handle::Server for File {
    fn read(
        self: Rc<Self>,
        params: file_handle::ReadParams,
        mut results: file_handle::ReadResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let request = params.get()?;
            let (offset, max_len) = (request.get_offset(), request.get_max_len());
            let path = self.path.clone();
            let bytes = self
                .server
                .call(move |backend| backend.read(&path, offset, max_len.min(MAX_READ_CHUNK)))
                .await?;
            results.get().set_bytes(&bytes);
            Ok(())
        })
    }

    fn write(
        self: Rc<Self>,
        _: file_handle::WriteParams,
        _: file_handle::WriteResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(read_only())
    }

    fn flush(
        self: Rc<Self>,
        _: file_handle::FlushParams,
        _: file_handle::FlushResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }

    fn metadata(
        self: Rc<Self>,
        _: file_handle::MetadataParams,
        mut results: file_handle::MetadataResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let path = self.path.clone();
            let metadata = self.server.call(move |backend| backend.stat(&path)).await?;
            write_metadata(results.get().init_metadata(), &metadata);
            Ok(())
        })
    }
}
