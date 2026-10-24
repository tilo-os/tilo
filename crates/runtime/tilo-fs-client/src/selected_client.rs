//! Bounded bridge to the existing filesystem capability on a connected fd.
//! The Cap'n Proto client stays on its `LocalSet`; FUSE never receives source fds.

use crate::{Metadata, OpenMode};
use std::{
    collections::HashMap,
    io,
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

const LIMIT: usize = 16;
const DEADLINE: Duration = Duration::from_secs(10);
const FILE: &str = crate::SELECTED_FILE;

enum Request {
    Check(u64),
    Stat,
    HandleStat(u64),
    Open(OpenMode),
    Read(u64, u64, u32),
    Write(u64, u64, Vec<u8>),
    Append(u64, Vec<u8>),
    Flush(u64),
    Release(u64),
}
enum Response {
    Metadata(Metadata),
    Handle(u64),
    Bytes(Vec<u8>),
    Written(u32),
    Empty,
}
struct Call {
    request: Request,
    answer: mpsc::SyncSender<io::Result<Response>>,
}

struct Inner {
    calls: Option<tokio::sync::mpsc::Sender<Call>>,
    transport: UnixStream,
    live: Arc<AtomicBool>,
    fault: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<io::Result<()>>>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
        drop(self.calls.take());
        let thread = match self.thread.get_mut() {
            Ok(thread) => thread,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(thread) = thread.take() {
            // Never in paint/layout: the projection owner retires off the UI.
            if let Err(error) = join_worker(thread) {
                eprintln!("retained source retirement failed: {error}");
            }
        }
    }
}

#[derive(Clone)]
pub struct SelectedFileConnection(Arc<Inner>);

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Retained source is unavailable",
    )
}

fn join_worker(thread: JoinHandle<io::Result<()>>) -> io::Result<()> {
    thread
        .join()
        .map_err(|_| io::Error::other("retained source worker panicked"))?
}

impl SelectedFileConnection {
    /// Close the queue and observe the exact RPC worker and transport outcome.
    /// Requires sole custody after all FUSE callbacks and handles have retired.
    pub fn retire(self) -> io::Result<()> {
        let mut inner = Arc::try_unwrap(self.0)
            .map_err(|_| io::Error::other("retained source still has active owners"))?;
        inner.live.store(false, Ordering::Release);
        drop(inner.calls.take());
        let thread = inner
            .thread
            .get_mut()
            .map_err(|_| io::Error::other("retained source worker lock poisoned"))?
            .take()
            .ok_or_else(|| io::Error::other("retained source already retired"))?;
        join_worker(thread)
    }
    /// Terminally close this exact authenticated channel before waiting for
    /// callbacks. The source provider observes peer death at native effect gates.
    pub fn revoke(&self) {
        self.0.live.store(false, Ordering::Release);
        let _ = self.0.transport.shutdown(std::net::Shutdown::Both);
    }
    pub fn connected(stream: UnixStream) -> io::Result<Self> {
        let transport = stream.try_clone()?;
        let (calls, receive) = tokio::sync::mpsc::channel::<Call>(LIMIT);
        let live = Arc::new(AtomicBool::new(true));
        let worker_live = Arc::clone(&live);
        let fault = Arc::new(AtomicBool::new(false));
        let worker_fault = Arc::clone(&fault);
        let (started, ready) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("retained-source-fs".into())
            .spawn(move || run_worker(stream, receive, worker_live, worker_fault, started))?;
        let inner = Arc::new(Inner {
            calls: Some(calls),
            transport,
            live,
            fault,
            thread: Mutex::new(Some(thread)),
        });
        ready
            .recv_timeout(DEADLINE.saturating_add(Duration::from_secs(1)))
            .map_err(|_| refused())??;
        Ok(Self(inner))
    }
    fn call(&self, request: Request) -> io::Result<Response> {
        self.call_with(|| request)
    }
    fn call_with(&self, request: impl FnOnce() -> Request) -> io::Result<Response> {
        if !self.0.live.load(Ordering::Acquire) {
            return Err(refused());
        }
        let (answer, receive) = mpsc::sync_channel(1);
        let permit = self
            .0
            .calls
            .as_ref()
            .ok_or_else(refused)?
            .try_reserve()
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        // Finite admission precedes cloning a potentially 1 MiB payload.
        permit.send(Call {
            request: request(),
            answer,
        });
        receive
            .recv_timeout(DEADLINE.saturating_add(Duration::from_secs(1)))
            .map_err(|_| {
                self.0.live.store(false, Ordering::Release);
                self.0.fault.store(true, Ordering::Release);
                refused()
            })?
    }
    /// # Errors
    /// Refuses revoked source handle scope or transport loss.
    pub fn check_scope(&self, id: u64) -> io::Result<()> {
        match self.call(Request::Check(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        match self.call(Request::Stat)? {
            Response::Metadata(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn metadata_handle(&self, id: u64) -> io::Result<Metadata> {
        match self.call(Request::HandleStat(id))? {
            Response::Metadata(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn open(&self, mode: OpenMode) -> io::Result<u64> {
        match self.call(Request::Open(mode))? {
            Response::Handle(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn read(&self, id: u64, offset: u64, len: u32) -> io::Result<Vec<u8>> {
        if len > crate::MAX_READ_CHUNK {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        match self.call(Request::Read(id, offset, len))? {
            Response::Bytes(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn write(&self, id: u64, offset: u64, bytes: &[u8]) -> io::Result<u32> {
        if bytes.len() > crate::MAX_WRITE_CHUNK as usize {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        match self.call_with(|| Request::Write(id, offset, bytes.to_vec()))? {
            Response::Written(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    /// One non-idempotent append; a failed transport may have an unknown effect.
    pub fn append(&self, id: u64, bytes: &[u8]) -> io::Result<u32> {
        if bytes.len() > crate::MAX_WRITE_CHUNK as usize {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        match self.call_with(|| Request::Append(id, bytes.to_vec()))? {
            Response::Written(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn flush(&self, id: u64) -> io::Result<()> {
        match self.call(Request::Flush(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    pub fn release(&self, id: u64) -> io::Result<()> {
        match self.call(Request::Release(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
}

// Boxed futures deliberately omit Send: capability clients stay on the local worker task.
type LocalFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

// Capability clients remain confined to the worker's local task throughout each request.
struct WorkerState {
    handles: HashMap<u64, crate::FileHandle>,
    next: u64,
    client: crate::FsClient,
}
impl WorkerState {
    fn check(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .check_scope()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn handle_stat(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .metadata()
                .await
                .map(Response::Metadata)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn stat(&self) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.client
                .stat(FILE)
                .await
                .map(Response::Metadata)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn open(&mut self, mode: OpenMode) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.handles.len() >= LIMIT || self.next == u64::MAX {
                return Err(io::Error::other("retained handle limit"));
            }
            let handle = self
                .client
                .open(FILE, mode)
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.handles.insert(self.next, handle);
            Ok(Response::Handle(self.next))
        })
    }
    fn read(&mut self, id: u64, offset: u64, len: u32) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .read_at(offset, len)
                .await
                .map(Response::Bytes)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn write(&self, id: u64, offset: u64, bytes: Vec<u8>) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .write_at(offset, &bytes)
                .await
                .map(Response::Written)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn append(&self, id: u64, bytes: Vec<u8>) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .append(&bytes)
                .await
                .map(Response::Written)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn flush(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .flush()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn release(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.handles.remove(&id).is_none() {
                return Err(refused());
            }
            Ok(Response::Empty)
        })
    }
    fn dispatch(&mut self, request: Request) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            match request {
                Request::Check(id) => self.check(id).await,
                Request::HandleStat(id) => self.handle_stat(id).await,
                Request::Stat => self.stat().await,
                Request::Open(mode) => self.open(mode).await,
                Request::Read(id, offset, len) => self.read(id, offset, len).await,
                Request::Write(id, offset, bytes) => self.write(id, offset, bytes).await,
                Request::Append(id, bytes) => self.append(id, bytes).await,
                Request::Flush(id) => self.flush(id).await,
                Request::Release(id) => self.release(id).await,
            }
        })
    }
}

fn run_worker(
    stream: UnixStream,
    receive: tokio::sync::mpsc::Receiver<Call>,
    worker_live: Arc<AtomicBool>,
    worker_fault: Arc<AtomicBool>,
    started: mpsc::SyncSender<io::Result<()>>,
) -> io::Result<()> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = started.send(Err(error));
            return Err(refused());
        }
    };
    tokio::task::LocalSet::new().block_on(
        &runtime,
        run_rpc(stream, receive, worker_live, worker_fault, started),
    )
}

fn run_rpc(
    stream: UnixStream,
    receive: tokio::sync::mpsc::Receiver<Call>,
    worker_live: Arc<AtomicBool>,
    worker_fault: Arc<AtomicBool>,
    started: mpsc::SyncSender<io::Result<()>>,
) -> LocalFuture<'static, io::Result<()>> {
    Box::pin(async move {
        use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
        stream.set_nonblocking(true)?;
        let stream = tokio::net::UnixStream::from_std(stream)?;
        let (read, write) = stream.into_split();
        let network = capnp_rpc::twoparty::VatNetwork::new(
            read.compat(),
            write.compat_write(),
            capnp_rpc::rpc_twoparty_capnp::Side::Client,
            capnp::message::ReaderOptions::default(),
        );
        let mut rpc = capnp_rpc::RpcSystem::new(Box::new(network), None);
        let raw: crate::WorkspaceFsClient =
            rpc.bootstrap(capnp_rpc::rpc_twoparty_capnp::Side::Server);
        let disconnect = rpc.get_disconnector();
        let mut driver = tokio::task::spawn_local(rpc);
        let client = crate::FsClient::from_raw(raw);
        let outcome = serve(client, receive, &worker_live, &worker_fault, started).await;
        worker_live.store(false, Ordering::Release);
        // The driver is owned here, never detached by bootstrap().
        // Graceful disconnect drains transport; cancellation is observed.
        let disconnect_result = tokio::time::timeout(DEADLINE, disconnect).await;
        let transport = if let Ok(result) = tokio::time::timeout(DEADLINE, &mut driver).await {
            result.map_err(io::Error::other)?.map_err(io::Error::other)
        } else {
            driver.abort();
            let cancelled = driver.await;
            Err(io::Error::other(format!(
                "retained RPC driver did not stop: {cancelled:?}"
            )))
        };
        outcome?;
        disconnect_result
            .map_err(|_| io::Error::other("retained RPC disconnect timed out"))?
            .map_err(io::Error::other)?;
        transport
    })
}

fn serve<'a>(
    client: crate::FsClient,
    mut receive: tokio::sync::mpsc::Receiver<Call>,
    worker_live: &'a AtomicBool,
    worker_fault: &'a AtomicBool,
    started: mpsc::SyncSender<io::Result<()>>,
) -> LocalFuture<'a, io::Result<()>> {
    Box::pin(async move {
        let initial = tokio::time::timeout(DEADLINE, client.stat(FILE)).await;
        match initial {
            Ok(Ok(metadata)) if metadata.kind == crate::EntryKind::File => {
                let _ = started.send(Ok(()));
            }
            _ => {
                worker_live.store(false, Ordering::Release);
                let _ = started.send(Err(refused()));
                return Err(refused());
            }
        }
        let mut state = WorkerState {
            handles: HashMap::new(),
            next: 0,
            client,
        };
        let mut terminal = None;
        while let Some(call) = receive.recv().await {
            if !worker_live.load(Ordering::Acquire) {
                let _ = call.answer.send(Err(refused()));
                continue;
            }
            let work = state.dispatch(call.request);
            let answer = match tokio::time::timeout(DEADLINE, work).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(error)) if crate::native_reply::is_operation_refusal(&error) => Err(error),
                Ok(Err(error)) => {
                    worker_live.store(false, Ordering::Release);
                    terminal = Some(io::Error::new(error.kind(), error.to_string()));
                    Err(error)
                }
                // A timed-out write has an unknown outcome. Never replay it.
                Err(_) => {
                    worker_live.store(false, Ordering::Release);
                    terminal = Some(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "retained source request timed out; outcome unknown",
                    ));
                    Err(refused())
                }
            };
            let _ = call.answer.send(answer);
        }
        match terminal {
            Some(error) => Err(error),
            None if worker_fault.load(Ordering::Acquire) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "retained caller lost request outcome",
            )),
            None => Ok(()),
        }
    })
}
