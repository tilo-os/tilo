//! Root-owned relay for an already admitted selected-file source capability.
//! The target receives only the existing filesystem RPC, never the worker fd.
use super::installed_retained_actor::InstalledRetainedLaunchInput;
use super::retained_selected_edit::RootSelectedEditor;
#[path = "retained_selected_parent.rs"]
pub mod selected_parent;
#[path = "retained_source_origin.rs"]
mod source_origin;
use launcher::owned_service::{TargetProjectionEndpoint, TargetProjectionMode};
use runtime_fs::{
    DirEntry, EntryKind, EntryName, Metadata, OpenMode,
    directory_backend::{
        DirectoryRows, DirectoryStat, JobPoll, WorkspaceDirectoryBackend, WorkspaceJobBackend,
        WorkspaceMetadataBackend,
    },
    directory_connection::{DirectoryConnection, ROOT_DIRECTORY},
    selected_client::SelectedFileConnection,
    workspace_server::{self, ConnectedWorkspaceServer, WorkspaceBackend, WorkspaceFileBackend},
};
pub(in crate::setup) use source_origin::{SourceHistoryReadOrigin, SourceSelectedOrigin};
use std::{
    io,
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const FILE: &str = runtime_fs::selected_file::SELECTED_FILE;
fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Retained source relay refused",
    )
}
// Constructor private: neither RPC labels nor caller-selected callbacks can
// replace installed custody. The lock serializes proof challenges and source
// calls; the connected server admits only eight pending backend callbacks.
// This is native owned-child delegation, not a second person proof. The
// originating certified Shell binding is never rewritten to the worker PID.
struct RetainedProjectionConsumer {
    endpoint: Arc<TargetProjectionEndpoint>,
}
impl RetainedProjectionConsumer {
    fn current(&self) -> io::Result<()> {
        self.endpoint.check()
    }
    fn connect(&self) -> io::Result<UnixStream> {
        self.current()?;
        let stream = self.endpoint.connect_control()?;
        self.current()?;
        Ok(stream)
    }
}
enum Consumer {
    SelectedEditor(Arc<RootSelectedEditor>),
    Projection(RetainedProjectionConsumer),
}
#[derive(Clone)]
enum Source {
    Selected(SelectedFileConnection),
    Home(DirectoryConnection),
}
impl Source {
    fn mode(&self) -> TargetProjectionMode {
        match self {
            Self::Selected(_) => TargetProjectionMode::SelectedFile,
            Self::Home(_) => TargetProjectionMode::HomeDirectory,
        }
    }
    fn home(&self) -> io::Result<&DirectoryConnection> {
        match self {
            Self::Home(source) => Ok(source),
            _ => Err(refused()),
        }
    }
    fn revoke(&self) {
        match self {
            Self::Selected(s) => s.revoke(),
            Self::Home(s) => s.revoke(),
        }
    }
    fn retire(self) -> io::Result<()> {
        match self {
            Self::Selected(s) => s.retire(),
            Self::Home(s) => s.retire(),
        }
    }
    fn metadata(&self) -> io::Result<Metadata> {
        match self {
            Self::Selected(s) => s.metadata(),
            Self::Home(s) => s.metadata(ROOT_DIRECTORY).map(|stat| stat.metadata),
        }
    }
    fn open(&self, mode: OpenMode) -> io::Result<u64> {
        match self {
            Self::Selected(s) => s.open(mode),
            _ => Err(refused()),
        }
    }
    fn metadata_handle(&self, id: u64) -> io::Result<Metadata> {
        match self {
            Self::Selected(s) => s.metadata_handle(id),
            Self::Home(s) => s.metadata_handle(id),
        }
    }
    fn check_scope(&self, id: u64) -> io::Result<()> {
        match self {
            Self::Selected(s) => s.check_scope(id),
            Self::Home(s) => s.check_scope(id),
        }
    }
    fn read(&self, id: u64, offset: u64, len: u32) -> io::Result<Vec<u8>> {
        match self {
            Self::Selected(s) => s.read(id, offset, len),
            Self::Home(s) => s.read(id, offset, len),
        }
    }
    fn write(&self, id: u64, offset: u64, bytes: &[u8]) -> io::Result<u32> {
        match self {
            Self::Selected(s) => s.write(id, offset, bytes),
            Self::Home(s) => s.write(id, offset, bytes),
        }
    }
    fn append(&self, id: u64, bytes: &[u8]) -> io::Result<u32> {
        match self {
            Self::Selected(s) => s.append(id, bytes),
            Self::Home(s) => s.append(id, bytes),
        }
    }
    fn flush(&self, id: u64) -> io::Result<()> {
        match self {
            Self::Selected(s) => s.flush(id),
            Self::Home(s) => s.flush(id),
        }
    }
    fn release(&self, id: u64) -> io::Result<()> {
        match self {
            Self::Selected(s) => s.release(id),
            Self::Home(s) => s.release(id),
        }
    }
}
struct Custody {
    input: Arc<InstalledRetainedLaunchInput>,
    cancel: Arc<AtomicBool>,
    revoked: AtomicBool,
    source: Mutex<Option<Source>>,
    interrupt: Source,
    consumer: Consumer,
    history: Mutex<Option<Arc<super::retained_history_read::RootHistoryReadSession>>>,
}
/// Destruction of one already-owned capability; never an effect or access request.
#[derive(Clone, Copy)]
enum ControllerRelease {
    Job(u64),
    Pin(u64),
    Pending(u64),
    Read(u64),
}
impl ControllerRelease {
    fn release(self, home: &DirectoryConnection) -> io::Result<()> {
        match self {
            Self::Job(id) => home.release_job(id),
            Self::Pin(id) => home.release_replacement_pin(id),
            Self::Pending(id) => home.release_pending_replacement(id),
            Self::Read(id) => home.release_history_read(id),
        }
    }
}
enum ControllerCleanup {
    Release(ControllerRelease),
    PendingHandoff { id: u64, ticket: [u8; 16] },
    ReadHandoff { id: u64, ticket: [u8; 16] },
    Barrier,
}
impl ControllerCleanup {
    fn perform(self, home: &DirectoryConnection) -> io::Result<()> {
        match self {
            Self::Release(release) => release.release(home),
            Self::PendingHandoff { id, ticket } => home.retire_pending_handoff(id, ticket),
            Self::ReadHandoff { id, ticket } => home.retire_history_read_handoff(id, ticket),
            // Fixed original-channel ordering observation, with no path supplied
            // by the consumer and no metadata returned to it.
            Self::Barrier => home.metadata(ROOT_DIRECTORY).map(|_| ()),
        }
    }
}
impl Custody {
    fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
        // No source-call lock: close the exact channel immediately even when
        // a backend callback holds custody while awaiting an actual effect.
        self.interrupt.revoke();
        self.input.revoke();
    }
    fn current(&self) -> io::Result<()> {
        if self.revoked.load(Ordering::Acquire)
            || rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
        {
            return Err(refused());
        }
        self.input.revalidate(&self.cancel)?;
        match &self.consumer {
            Consumer::SelectedEditor(editor) => editor.check()?,
            Consumer::Projection(consumer) => consumer.current()?,
        }
        Ok(())
    }
    /// Cleanup outlives the target's role/process proof. It may only release
    /// constructor-minted aliases/tickets on this exact still-owned connection.
    /// Native scope and observed callback retirement remain enforced remotely;
    /// any transport error is unknown and never acknowledged or replayed as IO.
    fn cleanup(&self, request: ControllerCleanup) -> io::Result<()> {
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(refused());
        }
        let source = self.source.lock().map_err(|_| refused())?;
        request.perform(source.as_ref().ok_or_else(refused)?.home()?)
    }
    fn call<T>(&self, call: impl FnOnce(&Source) -> io::Result<T>) -> io::Result<T> {
        let result: io::Result<T> = (|| {
            let source = self.source.lock().map_err(|_| refused())?;
            self.current()?;
            let value = call(source.as_ref().ok_or_else(refused)?);
            // A write followed by failed authority has an unknown outcome; it
            // is refused and terminal, and is never replayed by this relay.
            self.current()?;
            value
        })();
        if result
            .as_ref()
            .is_err_and(|error| !runtime_fs::native_reply::is_operation_refusal(error))
        {
            self.revoke();
        }
        result
    }
}
struct Backend(Arc<Custody>);
struct Handle {
    custody: Arc<Custody>,
    id: u64,
    identity: Option<[u64; 2]>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        // Revoked custody cannot issue further RPC. Connection retirement
        // below drops every remaining remote handle in either case.
        let _ = self.custody.call(|source| source.release(self.id));
    }
}
fn directory() -> Metadata {
    Metadata {
        kind: EntryKind::Directory,
        size: 0,
        modified_unix_seconds: None,
        needs_hydration: false,
    }
}
impl WorkspaceBackend for Backend {
    fn directory(&self) -> Result<Option<Arc<dyn WorkspaceDirectoryBackend>>, String> {
        let mode = self
            .0
            .call(|source| Ok(source.mode()))
            .map_err(|e| e.to_string())?;
        if mode == TargetProjectionMode::SelectedFile {
            return Ok(None);
        }
        Ok(Some(Arc::new(HomeDirectory {
            custody: self.0.clone(),
            id: ROOT_DIRECTORY,
        })))
    }
    fn list(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        let mode = self
            .0
            .call(|source| Ok(source.mode()))
            .map_err(|e| e.to_string())?;
        if mode != TargetProjectionMode::SelectedFile || !path.is_empty() {
            return Err("Selected-file root only".into());
        }
        self.0
            .call(|source| source.metadata())
            .map(|metadata| {
                vec![DirEntry {
                    name: FILE.into(),
                    metadata,
                }]
            })
            .map_err(|e| e.to_string())
    }
    fn stat(&self, path: &str) -> Result<Metadata, String> {
        let mode = self
            .0
            .call(|source| Ok(source.mode()))
            .map_err(|e| e.to_string())?;
        if mode != TargetProjectionMode::SelectedFile || (!path.is_empty() && path != FILE) {
            return Err("Unknown selected file".into());
        }
        self.0
            .call(|source| source.metadata())
            .map(|metadata| {
                if path.is_empty() {
                    directory()
                } else {
                    metadata
                }
            })
            .map_err(|e| e.to_string())
    }
    fn read(&self, path: &str, offset: u64, max_len: u32) -> Result<Vec<u8>, String> {
        let handle = self
            .open(path, OpenMode::Read)?
            .ok_or_else(|| "Missing selected handle".to_owned())?;
        handle.read(offset, max_len)
    }
    fn open(
        &self,
        path: &str,
        mode: OpenMode,
    ) -> Result<Option<Arc<dyn WorkspaceFileBackend>>, String> {
        let projection = self
            .0
            .call(|source| Ok(source.mode()))
            .map_err(|e| e.to_string())?;
        if projection != TargetProjectionMode::SelectedFile
            || path != FILE
            || matches!(mode, OpenMode::Append)
        {
            return Err("Unsupported selected-file open".into());
        }
        let id = self
            .0
            .call(|source| source.open(mode))
            .map_err(|e| e.to_string())?;
        Ok(Some(Arc::new(Handle {
            custody: Arc::clone(&self.0),
            id,
            identity: None,
        })))
    }
}
impl WorkspaceFileBackend for Handle {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(
            self.custody
                .call(|s| s.check_scope(self.id))
                .map_err(|e| e.to_string()),
        )
    }
    fn native_identity(&self) -> Result<Option<[u64; 2]>, String> {
        self.custody
            .call(|source| source.metadata_handle(self.id).map(|_| self.identity))
            .map_err(|e| e.to_string())
    }
    fn read(&self, offset: u64, max_len: u32) -> Result<Vec<u8>, String> {
        if max_len > runtime_fs::MAX_READ_CHUNK {
            return Err("Read exceeds bound".into());
        }
        self.custody
            .call(|source| source.read(self.id, offset, max_len))
            .map_err(|e| e.to_string())
    }
    fn write(&self, offset: u64, bytes: &[u8]) -> Result<u32, String> {
        if bytes.len() > runtime_fs::MAX_WRITE_CHUNK as usize {
            return Err("Write exceeds bound".into());
        }
        self.custody
            .call(|source| source.write(self.id, offset, bytes))
            .map_err(|e| e.to_string())
    }
    fn append(&self, bytes: &[u8]) -> Result<u32, String> {
        if bytes.len() > runtime_fs::MAX_WRITE_CHUNK as usize {
            return Err("Append exceeds bound".into());
        }
        self.custody
            .call(|source| source.append(self.id, bytes))
            .map_err(|e| e.to_string())
    }
    fn flush(&self) -> Result<(), String> {
        self.custody
            .call(|source| source.flush(self.id))
            .map_err(|e| e.to_string())
    }
    fn metadata(&self) -> Result<Metadata, String> {
        self.custody
            .call(|source| source.metadata_handle(self.id))
            .map_err(|e| e.to_string())
    }
}

/// Keep alongside the actual target namespace owner, then explicitly retire
/// after observed target death. This object has no wire/public constructor.
pub struct RetainedSourceRelay {
    custody: Option<Arc<Custody>>,
    server: Option<ConnectedWorkspaceServer>,
    runtime: Option<tokio::runtime::Runtime>,
}
impl RetainedSourceRelay {
    /// Metadata-only native provenance from this original Home relay. The
    /// caller retains its actual projection/Files lease and endpoint alongside
    /// the pin; an unrelated endpoint or FUSE identity cannot construct it.
    pub(in crate::setup) fn pin_home_selection(
        &self,
        components: &[EntryName],
    ) -> io::Result<Arc<SourceSelectedOrigin>> {
        let custody = self.custody.as_ref().ok_or_else(refused)?;
        custody.call(|source| {
            source.home()?;
            if source.metadata()?.kind != EntryKind::Directory {
                return Err(refused());
            }
            Ok(())
        })?;
        let root = Arc::new(HomeDirectory {
            custody: custody.clone(),
            id: ROOT_DIRECTORY,
        });
        SourceSelectedOrigin::pin(root, components)
    }
    /// The endpoint is minted only by the canonical launcher owner for its
    /// fixed root projection child. Caller streams or child PID labels cannot
    /// substitute for it. The caller retains that owner through relay retirement.
    pub fn serve_projection(
        source: SelectedFileConnection,
        input: Arc<InstalledRetainedLaunchInput>,
        cancel: Arc<AtomicBool>,
        endpoint: Arc<TargetProjectionEndpoint>,
    ) -> io::Result<Self> {
        Self::build(Source::Selected(source), input, cancel, endpoint)
    }
    pub fn serve_home_projection(
        source: DirectoryConnection,
        input: Arc<InstalledRetainedLaunchInput>,
        cancel: Arc<AtomicBool>,
        endpoint: Arc<TargetProjectionEndpoint>,
    ) -> io::Result<Self> {
        Self::build(Source::Home(source), input, cancel, endpoint)
    }
    pub fn serve_home_projection_with_history(
        source: DirectoryConnection,
        input: Arc<InstalledRetainedLaunchInput>,
        cancel: Arc<AtomicBool>,
        endpoint: Arc<TargetProjectionEndpoint>,
        history: Arc<super::retained_history_read::RootHistoryReadSession>,
    ) -> io::Result<Self> {
        if !history.uses_input(&input) {
            return Err(refused());
        }
        let relay = Self::serve_home_projection(source, input, cancel, endpoint)?;
        let custody = relay.custody.as_ref().ok_or_else(refused)?;
        custody.current()?;
        *custody.history.lock().map_err(|_| refused())? = Some(history);
        custody.current()?;
        Ok(relay)
    }
    fn build(
        source: Source,
        input: Arc<InstalledRetainedLaunchInput>,
        cancel: Arc<AtomicBool>,
        endpoint: Arc<TargetProjectionEndpoint>,
    ) -> io::Result<Self> {
        let consumer = Consumer::Projection(RetainedProjectionConsumer { endpoint });
        let custody = Arc::new(Custody {
            input,
            cancel,
            revoked: AtomicBool::new(false),
            interrupt: source.clone(),
            source: Mutex::new(Some(source)),
            consumer,
            history: Mutex::new(None),
        });
        let mut relay = Self {
            custody: Some(custody),
            server: None,
            runtime: None,
        };
        let result: io::Result<()> = (|| {
            let custody = relay.custody.as_ref().ok_or_else(refused)?;
            custody.current()?;
            let target = match &custody.consumer {
                Consumer::Projection(consumer) => consumer.connect()?,
                _ => return Err(refused()),
            };
            custody.call(|source| {
                if let Consumer::Projection(consumer) = &custody.consumer {
                    if consumer.endpoint.projection_mode() != source.mode() {
                        return Err(refused());
                    }
                }
                let expected = match source.mode() {
                    TargetProjectionMode::SelectedFile => EntryKind::File,
                    TargetProjectionMode::HomeDirectory => EntryKind::Directory,
                    TargetProjectionMode::SelectedParent => return Err(refused()),
                };
                if source.metadata()?.kind != expected {
                    return Err(refused());
                }
                Ok(())
            })?;
            relay.runtime = Some(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()?,
            );
            relay.server = Some(workspace_server::serve_workspace_connected(
                target,
                Arc::new(Backend(Arc::clone(custody))),
                relay.runtime.as_ref().ok_or_else(refused)?.handle().clone(),
            )?);
            Ok(())
        })();
        if let Err(error) = result {
            return match relay.stop() {
                Ok(()) => Err(error),
                Err(retirement) => Err(io::Error::other(format!(
                    "{error}; relay retirement failed: {retirement}"
                ))),
            };
        }
        Ok(relay)
    }
    pub fn retire_history(&self) -> io::Result<()> {
        let Some(custody) = self.custody.as_ref() else {
            return Ok(());
        };
        let mut reader = custody.history.lock().map_err(|_| refused())?;
        if let Some(history) = reader.as_ref() {
            history.retire()?;
            reader.take();
        }
        Ok(())
    }
    pub fn retire(mut self) -> io::Result<()> {
        self.stop()
    }
    fn stop(&mut self) -> io::Result<()> {
        let Some(custody) = self.custody.clone() else {
            return Ok(());
        };
        // Break the read-subject custody cycle before closing the original
        // source connection. Unknown query work remains owned until this lock
        // and the actual bounded daemon exchange have finished.
        {
            let mut reader = custody.history.lock().map_err(|_| refused())?;
            if let Some(history) = reader.as_ref() {
                history.retire()?;
                reader.take();
            }
        }
        // Retain the complete relay on any Reader retirement failure.
        self.custody.take();
        custody.revoke();
        let server_result = self
            .server
            .take()
            .map_or(Ok(()), ConnectedWorkspaceServer::retire);
        // Runtime drop waits for already spawned blocking callbacks. Do not
        // use shutdown_background/timeout: source custody must remain held.
        drop(self.runtime.take());
        let custody = Arc::try_unwrap(custody).map_err(|_| refused())?;
        let (mut source, lock_result) = match custody.source.into_inner() {
            Ok(source) => (source, Ok(())),
            Err(poisoned) => (poisoned.into_inner(), Err(refused())),
        };
        drop(custody.interrupt);
        let source_result = source.take().ok_or_else(refused)?.retire();
        server_result.and(lock_result).and(source_result)
    }
}
impl Drop for RetainedSourceRelay {
    fn drop(&mut self) {
        if self.stop().is_err() {
            // Unknown Reader/source retirement must not erase source epoch or
            // current proof custody when this cleanup owner unwinds.
            std::mem::forget(self.custody.take());
            std::mem::forget(self.server.take());
            std::mem::forget(self.runtime.take());
        }
    }
}

struct HomeDirectory {
    custody: Arc<Custody>,
    id: u64,
}
impl Drop for HomeDirectory {
    fn drop(&mut self) {
        if self.id != ROOT_DIRECTORY {
            let _ = self.custody.call(|s| s.home()?.release_directory(self.id));
        }
    }
}
impl WorkspaceDirectoryBackend for HomeDirectory {
    fn history_query(
        &self,
        name: &EntryName,
        expected: [u64; 2],
        query: &[u8],
    ) -> Result<Vec<u8>, String> {
        let result = (|| {
            self.custody.current()?;
            let history = self
                .custody
                .history
                .lock()
                .map_err(|_| refused())?
                .as_ref()
                .ok_or_else(refused)?
                .clone();
            let subject = self.history_read_subject(name).map_err(io::Error::other)?;
            // Do not hold Custody.source here: subject.check/observe themselves
            // use the same source-call gate, and daemon adoption calls them too.
            let bytes = history.query(subject, expected, query)?;
            self.custody.current()?;
            Ok(bytes)
        })();
        result.map_err(|error: io::Error| error.to_string())
    }
    fn history_read_subject(
        &self,
        name: &runtime_fs::EntryName,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceHistoryReadSubjectBackend>, String>
    {
        let id = self
            .custody
            .call(|source| source.home()?.history_read_subject(self.id, name))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(HomeHistoryReadSubject {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }

    fn sync_directory(&self) -> Result<(), String> {
        self.custody
            .call(|source| source.home()?.sync_directory(self.id))
            .map_err(|error| error.to_string())
    }

    fn prepare_replace(
        &self,
        temporary: &EntryName,
        expected: [u64; 2],
        victim: &EntryName,
        old: [u64; 2],
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspacePendingReplacementBackend>, String>
    {
        let id = self
            .custody
            .call(|source| {
                source
                    .home()?
                    .prepare_replacement(self.id, temporary, expected, victim, old)
            })
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(HomePendingReplacement {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }
    fn statfs(&self) -> Result<runtime_fs::directory_backend::DirectoryFilesystemStats, String> {
        self.custody
            .call(|s| s.home()?.statfs(self.id))
            .map_err(|e| e.to_string())
    }
    fn open_cursor(
        &self,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceDirectoryCursorBackend>, String>
    {
        let id = self
            .custody
            .call(|s| s.home()?.open_cursor(self.id))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(HomeCursor {
            custody: self.custody.clone(),
            id,
        }))
    }
    fn check(&self) -> Result<(), String> {
        self.custody
            .call(|s| s.home()?.metadata(self.id).map(|_| ()))
            .map_err(|e| e.to_string())
    }
    fn metadata(&self) -> Result<DirectoryStat, String> {
        self.custody
            .call(|s| s.home()?.metadata(self.id))
            .map_err(|e| e.to_string())
    }
    fn stat(&self, name: &EntryName) -> Result<Option<DirectoryStat>, String> {
        self.custody
            .call(|s| s.home()?.stat(self.id, name))
            .map_err(|e| e.to_string())
    }
    fn readlink(&self, name: &EntryName, expected: [u64; 2]) -> Result<Vec<u8>, String> {
        self.custody
            .call(|s| s.home()?.readlink(self.id, name, expected))
            .map_err(|e| e.to_string())
    }
    fn pin_metadata(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceMetadataBackend>, String> {
        let id = self
            .custody
            .call(|s| s.home()?.pin_metadata(self.id, name, expected))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(HomeMetadata {
            custody: self.custody.clone(),
            id,
        }))
    }
    fn list(&self, cursor: u64) -> Result<DirectoryRows, String> {
        self.custody
            .call(|s| s.home()?.list(self.id, cursor))
            .map_err(|e| e.to_string())
    }
    fn directory(&self, name: &EntryName) -> Result<Arc<dyn WorkspaceDirectoryBackend>, String> {
        let id = self
            .custody
            .call(|s| s.home()?.directory(self.id, name))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            custody: self.custody.clone(),
            id,
        }))
    }
    fn open_file(
        &self,
        name: &EntryName,
        mode: OpenMode,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceFileBackend>, String> {
        let id = self
            .custody
            .call(|s| s.home()?.open_expected(self.id, name, mode, expected))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Handle {
            custody: self.custody.clone(),
            id,
            identity: Some(expected),
        }))
    }
    fn create_file(&self, name: EntryName) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let id = self
            .custody
            .call(|s| s.home()?.create_file(self.id, &name))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(HomeJob {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }
    fn create_directory(&self, name: EntryName) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let id = self
            .custody
            .call(|s| s.home()?.create_directory(self.id, &name))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(HomeJob {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }

    fn recoveries(
        &self,
        after: &[u8],
    ) -> Result<runtime_fs::directory_backend::RecoveryPage, String> {
        self.custody
            .call(|source| source.home()?.recoveries(self.id, after))
            .map_err(|error| error.to_string())
    }
    fn restore_captured(
        &self,
        recovery: [u8; 16],
        name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let id = self
            .custody
            .call(|source| source.home()?.restore_captured(self.id, recovery, &name))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(HomeJob {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }
    fn atomic_save(
        &self,
        temporary: &EntryName,
        expected: [u64; 2],
        target: &EntryName,
    ) -> Result<(), String> {
        self.custody
            .call(|source| {
                source
                    .home()?
                    .atomic_save(self.id, temporary, expected, target)
            })
            .map_err(|error| error.to_string())
    }
    fn rename_replace(
        &self,
        name: &EntryName,
        new: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let id = self
            .custody
            .call(|s| s.home()?.rename_replace(self.id, name, &new))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(HomeJob {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }
    fn rename_no_replace(
        &self,
        name: &EntryName,
        new: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let id = self
            .custody
            .call(|s| s.home()?.rename_no_replace(self.id, name, &new))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(HomeJob {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }
}
struct HomeCursor {
    custody: Arc<Custody>,
    id: u64,
}
impl runtime_fs::directory_backend::WorkspaceDirectoryCursorBackend for HomeCursor {
    fn check(&self) -> Result<(), String> {
        self.custody
            .call(|s| s.home()?.metadata(ROOT_DIRECTORY).map(|_| ()))
            .map_err(|e| e.to_string())
    }
    fn page(
        &self,
        sequence: u64,
    ) -> Result<runtime_fs::directory_backend::DirectoryStreamPage, String> {
        self.custody
            .call(|s| s.home()?.cursor_page(self.id, sequence))
            .map_err(|e| e.to_string())
    }
}
impl Drop for HomeCursor {
    fn drop(&mut self) {
        let _ = self.custody.call(|s| s.home()?.release_cursor(self.id));
    }
}
struct HomeMetadata {
    custody: Arc<Custody>,
    id: u64,
}
impl WorkspaceMetadataBackend for HomeMetadata {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(
            self.custody
                .call(|s| s.home()?.check_pin(self.id))
                .map_err(|e| e.to_string()),
        )
    }
    fn metadata(&self) -> Result<DirectoryStat, String> {
        self.custody
            .call(|s| s.home()?.metadata_pin(self.id))
            .map_err(|e| e.to_string())
    }
}
impl Drop for HomeMetadata {
    fn drop(&mut self) {
        let _ = self.custody.call(|s| s.home()?.release_pin(self.id));
    }
}
/// Exact constructor-minted map alias release, with progress kept across an
/// unknown barrier reply. State1 has removed the map; only state2 is an ACK.
#[derive(Default)]
struct ControllerCache(Mutex<u8>);
impl ControllerCache {
    fn retire(&self, custody: &Custody, release: ControllerRelease) -> Result<(), String> {
        let mut state = self.0.lock().map_err(|_| "controller cache poisoned")?;
        if *state == 2 {
            return Ok(());
        }
        if *state == 0 {
            custody
                .cleanup(ControllerCleanup::Release(release))
                .map_err(|error| error.to_string())?;
            *state = 1;
        }
        // SAME ordered original source channel, after the actual local cap drop.
        // Unknown reply retains state1; retry only the fixed observation barrier.
        custody
            .cleanup(ControllerCleanup::Barrier)
            .map_err(|error| error.to_string())?;
        *state = 2;
        Ok(())
    }
    fn open(&self) -> bool {
        self.0.lock().is_ok_and(|state| *state == 0)
    }
}
struct HomeJob {
    cache: ControllerCache,
    custody: Arc<Custody>,
    id: u64,
}
impl Drop for HomeJob {
    fn drop(&mut self) {
        if !self.cache.open() {
            return;
        }
        let _ = self.custody.call(|s| s.home()?.release_job(self.id));
    }
}
impl WorkspaceJobBackend for HomeJob {
    fn retire_controller_cache(&self) -> Result<(), String> {
        self.cache
            .retire(&self.custody, ControllerRelease::Job(self.id))
    }

    fn created_temporary(
        &self,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceCreatedTemporaryBackend>, String>
    {
        let id = self
            .custody
            .call(|source| source.home()?.created_temporary(self.id))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(HomeCreatedTemporary {
            custody: self.custody.clone(),
            id,
        }))
    }

    fn pin_replacement(
        &self,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceReplacementPinBackend>, String>
    {
        let id = self
            .custody
            .call(|s| s.home()?.pin_replacement(self.id))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(HomeReplacementPin {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(
            self.custody
                .call(|s| s.home()?.check_job(self.id))
                .map_err(|e| e.to_string()),
        )
    }
    fn poll(&self) -> Result<JobPoll, String> {
        self.custody
            .call(|s| s.home()?.poll(self.id))
            .map_err(|e| e.to_string())
    }
    fn cancel(&self) -> Result<(), String> {
        self.custody
            .call(|s| s.home()?.cancel(self.id))
            .map_err(|e| e.to_string())
    }
}

/// A connected target service receives only the admitted selected inode and
/// this editor's own completed CreateFile temporaries. Plain Home backends and
/// public caller callbacks cannot construct this relay.
pub struct RetainedSelectedEditRelay {
    owner: Option<Arc<Mutex<selected_parent::RootSelectedEditSource>>>,
    admission: Option<Arc<super::retained_selected_edit::RootSelectedEditAdmission>>,
    endpoint: Option<Arc<TargetProjectionEndpoint>>,
    filtered: Option<Arc<selected_parent::SelectedParent>>,
    server: Option<ConnectedWorkspaceServer>,
    runtime: Option<tokio::runtime::Runtime>,
    stopped: bool,
    callbacks_unknown: bool,
    retirement: Option<Result<(), String>>,
}
struct SelectedEditBackend {
    filtered: Arc<selected_parent::SelectedParent>,
}
impl WorkspaceBackend for SelectedEditBackend {
    fn directory(&self) -> Result<Option<Arc<dyn WorkspaceDirectoryBackend>>, String> {
        self.filtered.check()?;
        Ok(Some(self.filtered.clone()))
    }
    fn open(&self, _: &str, _: OpenMode) -> Result<Option<Arc<dyn WorkspaceFileBackend>>, String> {
        self.filtered.check()?;
        Err("Selected-parent legacy paths refused".into())
    }
    fn list(&self, _: &str) -> Result<Vec<DirEntry>, String> {
        self.filtered.check()?;
        Err("Selected-parent legacy paths refused".into())
    }
    fn stat(&self, _: &str) -> Result<Metadata, String> {
        self.filtered.check()?;
        Err("Selected-parent legacy paths refused".into())
    }
    fn read(&self, _: &str, _: u64, _: u32) -> Result<Vec<u8>, String> {
        self.filtered.check()?;
        Err("Selected-parent legacy paths refused".into())
    }
}
impl RetainedSelectedEditRelay {
    pub(in crate::setup) fn serve(
        owner: Arc<Mutex<selected_parent::RootSelectedEditSource>>,
        source: Arc<selected_parent::SourceSelectedParent>,
        admission: Arc<super::retained_selected_edit::RootSelectedEditAdmission>,
        endpoint: Arc<TargetProjectionEndpoint>,
    ) -> io::Result<Self> {
        let mut relay = Self {
            owner: Some(owner.clone()),
            admission: Some(admission.clone()),
            endpoint: Some(endpoint),
            filtered: None,
            server: None,
            runtime: None,
            stopped: false,
            callbacks_unknown: false,
            retirement: None,
        };
        let result: io::Result<()> = (|| {
            if !admission.uses_source(&source) {
                return Err(refused());
            }
            let filtered = Arc::new(
                selected_parent::SelectedParent::new(source, admission, owner)
                    .map_err(io::Error::other)?,
            );
            relay.filtered = Some(filtered.clone());
            relay.current()?;
            let stream = relay
                .endpoint
                .as_ref()
                .ok_or_else(refused)?
                .connect_control()?;
            relay.current()?;
            relay.runtime = Some(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()?,
            );
            relay.server = Some(workspace_server::serve_workspace_connected(
                stream,
                Arc::new(SelectedEditBackend { filtered }),
                relay.runtime.as_ref().ok_or_else(refused)?.handle().clone(),
            )?);
            relay.current()?;
            Ok(())
        })();
        if let Err(error) = result {
            let cleanup = relay.retire();
            return Err(cleanup.err().unwrap_or(error));
        }
        Ok(relay)
    }
    fn current(&self) -> io::Result<()> {
        if self.stopped {
            return Err(refused());
        }
        let admission = self.admission.as_ref().ok_or_else(refused)?;
        admission.check()?;
        {
            let owner = self
                .owner
                .as_ref()
                .ok_or_else(refused)?
                .lock()
                .map_err(|_| refused())?;
            if !owner.uses_editor(admission.editor()) {
                return Err(refused());
            }
            owner.check_current()?;
        }
        let endpoint = self.endpoint.as_ref().ok_or_else(refused)?;
        if endpoint.projection_mode() != TargetProjectionMode::SelectedParent {
            return Err(refused());
        }
        endpoint.check()?;
        self.filtered
            .as_ref()
            .ok_or_else(refused)?
            .check()
            .map_err(io::Error::other)?;
        admission.check()
    }
    /// Observe only connected RPC/callback teardown. This is not evidence that
    /// either target or source namespace has stopped.
    pub fn observed_stop(&self) -> bool {
        self.stopped
    }
    /// Parent retires target namespace first; this method drains relay work and
    /// releases its filter capabilities. Parent then drops remaining target
    /// authority/admission/source refs before source_owner.retire, off UI.
    pub fn retire(&mut self) -> io::Result<()> {
        if self.callbacks_unknown {
            return Err(io::Error::other(
                "Selected edit callbacks have unknown retirement custody",
            ));
        }
        if let Some(result) = &self.retirement {
            return result
                .as_ref()
                .copied()
                .map_err(|error| io::Error::other(error.clone()));
        }
        if let Some(filtered) = &self.filtered {
            filtered.revoke();
        }
        if let Some(admission) = &self.admission {
            admission.editor().revoke();
        }
        // Runtime drop waits for native blocking calls and cannot be performed
        // from inside an async runtime. Refusal retains all exact custody.
        if self.runtime.is_some() && tokio::runtime::Handle::try_current().is_ok() {
            return Err(io::Error::other(
                "Retire selected edit relay outside async runtime",
            ));
        }
        let server_result = self
            .server
            .take()
            .map_or(Ok(()), ConnectedWorkspaceServer::retire);
        let runtime_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(self.runtime.take())));
        if runtime_result.is_err() {
            self.callbacks_unknown = true;
            return Err(io::Error::other(
                "Selected edit callback runtime retirement was not observed",
            ));
        }
        // The connected thread has joined (including an error/panic outcome)
        // and all supplied-runtime blocking callbacks have actually returned.
        self.stopped = true;
        self.filtered.take();
        self.endpoint.take();
        self.admission.take();
        self.owner.take();
        self.retirement = Some(server_result.as_ref().copied().map_err(ToString::to_string));
        server_result
    }
}
impl Drop for RetainedSelectedEditRelay {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        if let Some(filtered) = &self.filtered {
            filtered.revoke();
        }
        if let Some(admission) = &self.admission {
            admission.editor().revoke();
        }
        // A dropped owner cannot claim stopped callbacks or release namespace
        // custody. The root driver must retain and explicitly retire this relay.
        if let Some(server) = self.server.take() {
            std::mem::forget(server);
        }
        if let Some(runtime) = self.runtime.take() {
            std::mem::forget(runtime);
        }
        if let Some(filtered) = self.filtered.take() {
            std::mem::forget(filtered);
        }
        if let Some(endpoint) = self.endpoint.take() {
            std::mem::forget(endpoint);
        }
        if let Some(admission) = self.admission.take() {
            std::mem::forget(admission);
        }
        if let Some(owner) = self.owner.take() {
            std::mem::forget(owner);
        }
    }
}

/// Same authenticated source channel and same installed/consumer epoch as the job.
/// This wrapper cannot commit history ownership; that requires daemon enrollment.
struct HomeReplacementPin {
    cache: ControllerCache,
    custody: Arc<Custody>,
    id: u64,
}
impl Drop for HomeReplacementPin {
    fn drop(&mut self) {
        if !self.cache.open() {
            return;
        }
        let _ = self
            .custody
            .call(|s| s.home()?.release_replacement_pin(self.id));
    }
}
impl runtime_fs::directory_backend::WorkspaceReplacementPinBackend for HomeReplacementPin {
    fn retire_controller_cache(&self) -> Result<(), String> {
        self.cache
            .retire(&self.custody, ControllerRelease::Pin(self.id))
    }

    fn commit_with_history(&self, receipt: &[u8]) -> Result<(), String> {
        self.custody
            .call(|source| source.home()?.commit_replacement_pin(self.id, receipt))
            .map_err(|error| error.to_string())
    }

    fn check(&self) -> Result<(), String> {
        self.custody
            .call(|s| s.home()?.check_replacement_pin(self.id))
            .map_err(|e| e.to_string())
    }
    fn observe(&self) -> Result<runtime_fs::directory_backend::ReplacementObservation, String> {
        self.custody
            .call(|s| s.home()?.observe_replacement_pin(self.id))
            .map_err(|e| e.to_string())
    }
}

/// Holds only the real pending source capability through the same root custody.
/// No source stream, native grant, signer or serialized authority is handed out.
struct HomePendingReplacement {
    cache: ControllerCache,
    custody: Arc<Custody>,
    id: u64,
}
impl runtime_fs::directory_backend::WorkspacePendingReplacementBackend for HomePendingReplacement {
    fn retire_controller_cache(&self) -> Result<(), String> {
        self.cache
            .retire(&self.custody, ControllerRelease::Pending(self.id))
    }

    fn acknowledged_committed_job(&self) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let id = self
            .custody
            .call(|source| source.home()?.acknowledged_committed_job(self.id))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(HomeJob {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }

    fn acknowledged_committed_pin(
        &self,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspaceReplacementPinBackend>, String>
    {
        let id = self
            .custody
            .call(|source| source.home()?.acknowledged_committed_pin(self.id))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(HomeReplacementPin {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }
    fn retire_daemon_handoff(&self, ticket: [u8; 16]) -> Result<(), String> {
        self.custody
            .cleanup(ControllerCleanup::PendingHandoff {
                id: self.id,
                ticket,
            })
            .map_err(|error| error.to_string())
    }
    fn arm_daemon_handoff(&self, challenge: [u8; 32]) -> Result<[u8; 16], String> {
        self.custody
            .call(|source| source.home()?.arm_pending_handoff(self.id, challenge))
            .map_err(|error| error.to_string())
    }

    fn check(&self) -> Result<(), String> {
        self.custody
            .call(|source| source.home()?.check_pending_replacement(self.id))
            .map_err(|error| error.to_string())
    }
    fn observe(
        &self,
    ) -> Result<runtime_fs::directory_backend::PendingReplacementObservation, String> {
        self.custody
            .call(|source| source.home()?.observe_pending_replacement(self.id))
            .map_err(|error| error.to_string())
    }
    fn start_with_history(&self, receipt: &[u8]) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let id = self
            .custody
            .call(|source| source.home()?.start_pending_replacement(self.id, receipt))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(HomeJob {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }
    fn cancel(&self) -> Result<(), String> {
        self.custody
            .call(|source| source.home()?.cancel_pending_replacement(self.id))
            .map_err(|error| error.to_string())
    }
}
impl Drop for HomePendingReplacement {
    fn drop(&mut self) {
        if !self.cache.open() {
            return;
        }
        let _ = self
            .custody
            .call(|source| source.home()?.release_pending_replacement(self.id));
    }
}

// Source-resident, actual creator-job capability; no path/receipt constructor.
struct HomeCreatedTemporary {
    custody: Arc<Custody>,
    id: u64,
}
impl Drop for HomeCreatedTemporary {
    fn drop(&mut self) {
        let _ = self
            .custody
            .call(|source| source.home()?.release_created_temporary(self.id));
    }
}
impl runtime_fs::directory_backend::WorkspaceCreatedTemporaryBackend for HomeCreatedTemporary {
    fn set_permissions(&self, mode: u32) -> Result<(), String> {
        self.custody
            .call(|source| source.home()?.set_temporary_permissions(self.id, mode))
            .map_err(|error| error.to_string())
    }

    fn prepare_atomic_save(
        &self,
    ) -> runtime_fs::directory_backend::AtomicSavePreparation<
        Arc<dyn runtime_fs::directory_backend::WorkspacePendingReplacementBackend>,
    > {
        use runtime_fs::directory_backend::AtomicSavePreparation as Outcome;
        match self
            .custody
            .call(|source| Ok(source.home()?.prepare_created_atomic_save(self.id)))
        {
            Ok(Outcome::Prepared(id)) => Outcome::Prepared(Arc::new(HomePendingReplacement {
                cache: ControllerCache::default(),
                custody: self.custody.clone(),
                id,
            })),
            Ok(Outcome::Refused(error)) => Outcome::Refused(error),
            Ok(Outcome::Unknown(error)) => Outcome::Unknown(error),
            Err(error) => Outcome::Unknown(error.to_string()),
        }
    }

    fn prepare_replace(
        &self,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspacePendingReplacementBackend>, String>
    {
        let id = self
            .custody
            .call(|source| source.home()?.prepare_created_replacement(self.id))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(HomePendingReplacement {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }

    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(self.check())
    }
    fn check(&self) -> Result<(), String> {
        self.custody
            .call(|source| source.home()?.check_created_temporary(self.id))
            .map_err(|error| error.to_string())
    }
    fn retire(&self) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let id = self
            .custody
            .call(|source| source.home()?.retire_created_temporary(self.id))
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(HomeJob {
            cache: ControllerCache::default(),
            custody: self.custody.clone(),
            id,
        }))
    }
}

/// Actual source capability retained under the same installed Files custody.
/// This does not admit the daemon receiver or derive a canonical record ID.
struct HomeHistoryReadSubject {
    cache: ControllerCache,
    custody: Arc<Custody>,
    id: u64,
}
impl Drop for HomeHistoryReadSubject {
    fn drop(&mut self) {
        if !self.cache.open() {
            return;
        }
        let _ = self
            .custody
            .call(|source| source.home()?.release_history_read(self.id));
    }
}
impl runtime_fs::directory_backend::WorkspaceHistoryReadSubjectBackend for HomeHistoryReadSubject {
    fn retire_controller_cache(&self) -> Result<(), String> {
        self.cache
            .retire(&self.custody, ControllerRelease::Read(self.id))
    }

    fn retire_daemon_handoff(&self, ticket: [u8; 16]) -> Result<(), String> {
        self.custody
            .cleanup(ControllerCleanup::ReadHandoff {
                id: self.id,
                ticket,
            })
            .map_err(|error| error.to_string())
    }
    fn arm_daemon_handoff(&self, challenge: [u8; 32]) -> Result<[u8; 16], String> {
        self.custody
            .call(|source| source.home()?.arm_history_read_handoff(self.id, challenge))
            .map_err(|error| error.to_string())
    }

    fn check_scope(&self) -> Result<(), String> {
        self.custody
            .call(|source| source.home()?.check_history_read(self.id, false))
            .map_err(|error| error.to_string())
    }
    fn check(&self) -> Result<(), String> {
        self.custody
            .call(|source| source.home()?.check_history_read(self.id, true))
            .map_err(|error| error.to_string())
    }
    fn observe(&self) -> Result<runtime_fs::directory_backend::HistoryReadObservation, String> {
        self.custody
            .call(|source| source.home()?.observe_history_read(self.id))
            .map_err(|error| error.to_string())
    }
}
