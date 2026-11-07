//! The bounded bridge a capability client drives on a connected fd.
//!
//! The capability client stays on its own worker thread; FUSE never receives
//! source fds. Calls are admitted through one bounded queue, and a transport
//! that died answers every waiting call with a refusal instead of blocking.

use std::{
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

/// Requests one bridge admits at once, across every capability on it.
pub(crate) const LIMIT: usize = 16;
/// Bound on one request, and on the RPC shutdown that follows.
pub(crate) const DEADLINE: Duration = Duration::from_secs(10);

pub(crate) type LocalFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

/// One admitted request and where its answer goes.
pub(crate) struct Call<Request, Response> {
    pub(crate) request: Request,
    pub(crate) answer: mpsc::SyncSender<io::Result<Response>>,
}

/// A worker-side capability that answers this bridge's requests.
pub(crate) trait Dispatch<Request, Response> {
    fn dispatch(&mut self, request: Request) -> LocalFuture<'_, io::Result<Response>>;
}

/// A worker-side capability admitted by an initial handshake on its client.
pub(crate) trait Serve<Request, Response>: Dispatch<Request, Response> + Sized {
    /// The initial handshake that admits this capability, or why it is refused.
    fn admit(client: crate::FsClient) -> LocalFuture<'static, io::Result<Self>>;
}

struct Inner<Request, Response> {
    calls: Option<tokio::sync::mpsc::Sender<Call<Request, Response>>>,
    transport: UnixStream,
    live: Arc<AtomicBool>,
    fault: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<io::Result<()>>>>,
}

impl<Request, Response> Drop for Inner<Request, Response> {
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

/// One capability client on one authenticated stream.
pub(crate) struct Bridge<Request, Response>(Arc<Inner<Request, Response>>);

impl<Request, Response> Clone for Bridge<Request, Response> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

pub(crate) fn refused() -> io::Error {
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

impl<Request, Response> Bridge<Request, Response>
where
    Request: Send + 'static,
    Response: Send + 'static,
{
    /// Start the worker that serves `S` over `stream` and wait for its handshake.
    pub(crate) fn connect<S>(name: &str, stream: UnixStream) -> io::Result<Self>
    where
        S: Serve<Request, Response> + 'static,
    {
        let transport = stream.try_clone()?;
        let (calls, receive) = tokio::sync::mpsc::channel::<Call<Request, Response>>(LIMIT);
        let live = Arc::new(AtomicBool::new(true));
        let worker_live = Arc::clone(&live);
        let fault = Arc::new(AtomicBool::new(false));
        let worker_fault = Arc::clone(&fault);
        let (started, ready) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                run_worker::<Request, Response, S>(
                    stream,
                    receive,
                    worker_live,
                    worker_fault,
                    started,
                )
            })?;
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

    /// Close the queue and observe the exact worker and transport outcome.
    /// Requires sole custody after all FUSE callbacks and handles have retired.
    pub(crate) fn retire(self) -> io::Result<()> {
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
    pub(crate) fn revoke(&self) {
        self.0.live.store(false, Ordering::Release);
        let _ = self.0.transport.shutdown(std::net::Shutdown::Both);
    }

    /// Ask the worker one request; `request` runs only once it is admitted.
    pub(crate) fn call_with(&self, request: impl FnOnce() -> Request) -> io::Result<Response> {
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
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(()) => {
                    crate::native_reply::bounded_refusal()
                }
                tokio::sync::mpsc::error::TrySendError::Closed(()) => refused(),
            })?;
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
}

fn run_worker<Request, Response, S>(
    stream: UnixStream,
    receive: tokio::sync::mpsc::Receiver<Call<Request, Response>>,
    worker_live: Arc<AtomicBool>,
    worker_fault: Arc<AtomicBool>,
    started: mpsc::SyncSender<io::Result<()>>,
) -> io::Result<()>
where
    Request: Send + 'static,
    Response: Send + 'static,
    S: Serve<Request, Response> + 'static,
{
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
        run_rpc::<Request, Response, S>(stream, receive, worker_live, worker_fault, started),
    )
}

fn run_rpc<Request, Response, S>(
    stream: UnixStream,
    receive: tokio::sync::mpsc::Receiver<Call<Request, Response>>,
    worker_live: Arc<AtomicBool>,
    worker_fault: Arc<AtomicBool>,
    started: mpsc::SyncSender<io::Result<()>>,
) -> LocalFuture<'static, io::Result<()>>
where
    Request: Send + 'static,
    Response: Send + 'static,
    S: Serve<Request, Response>,
{
    Box::pin(async move {
        let connection = crate::connection::Connection::connect("retained-source-fs", stream)?;
        let client = crate::FsClient::from_object(crate::connection::Object::root(connection));
        let outcome =
            serve::<Request, Response, S>(client, receive, &worker_live, &worker_fault, started)
                .await;
        worker_live.store(false, Ordering::Release);
        outcome
    })
}

/// Admit the capability, then answer its requests until the queue closes.
fn serve<'a, Request, Response, S>(
    client: crate::FsClient,
    mut receive: tokio::sync::mpsc::Receiver<Call<Request, Response>>,
    worker_live: &'a AtomicBool,
    worker_fault: &'a AtomicBool,
    started: mpsc::SyncSender<io::Result<()>>,
) -> LocalFuture<'a, io::Result<()>>
where
    Request: Send + 'static,
    Response: Send + 'static,
    S: Serve<Request, Response>,
{
    Box::pin(async move {
        let admitted = tokio::time::timeout(DEADLINE, S::admit(client)).await;
        let Ok(Ok(mut state)) = admitted else {
            worker_live.store(false, Ordering::Release);
            let _ = started.send(Err(refused()));
            return Err(refused());
        };
        let _ = started.send(Ok(()));
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
