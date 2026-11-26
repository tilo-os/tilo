//! The app profile: one current-thread runtime on its own thread.

use std::future::Future;
use std::sync::OnceLock;
use std::thread::ThreadId;

use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::{Error, pool};

pub(crate) const APP_THREAD: &str = "tilo-rt";

/// The process-wide app runtime: a current-thread tokio runtime driven by a
/// dedicated thread.
///
/// One thread is enough for an app: its async work is network and IPC
/// waiting, not computation. CPU-bound work goes to [`crate::cpu()`] and
/// blocking calls to [`AppRuntime::spawn_blocking`], so neither stalls the
/// other tasks.
///
/// The runtime starts on first use and lives as long as the process.
#[derive(Debug)]
pub struct AppRuntime {
    handle: Handle,
    thread: ThreadId,
}

static APP: OnceLock<Result<AppRuntime, String>> = OnceLock::new();

#[allow(
    clippy::panic,
    reason = "a process that cannot start one thread cannot run an app; try_app reports it"
)]
pub fn app() -> &'static AppRuntime {
    match try_app() {
        Ok(runtime) => runtime,
        Err(error) => panic!("{error}"),
    }
}

pub fn try_app() -> Result<&'static AppRuntime, Error> {
    APP.get_or_init(|| AppRuntime::start().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|message| Error::Start(std::io::Error::other(message.clone())))
}

impl AppRuntime {
    fn start() -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .thread_name("tilo-rt-blocking")
            .max_blocking_threads(pool::APP_BLOCKING_THREADS)
            .build()?;
        let handle = runtime.handle().clone();
        let driver = crate::spawn_thread(APP_THREAD, move || {
            let _driving = crate::Driving::enter();
            runtime.block_on(std::future::pending::<()>());
        })?;
        Ok(Self {
            handle,
            thread: driver.thread().id(),
        })
    }

    pub const fn handle(&self) -> &Handle {
        &self.handle
    }

    pub const fn thread_id(&self) -> ThreadId {
        self.thread
    }

    pub fn is_runtime_thread(&self) -> bool {
        std::thread::current().id() == self.thread
    }

    /// Run `future` on the runtime thread. Returns immediately; the
    /// caller never polls it.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.handle.spawn(future)
    }

    /// Run `future` on the runtime thread, then hand its output to
    /// `deliver` there. `deliver` should only publish — complete a
    /// `LatestRequest`, send on a channel — so the runtime thread is free
    /// for the next task. This is the UI handoff: the UI thread spawns, and
    /// `deliver` wakes the event loop with the result.
    pub fn spawn_then<F, D>(&self, future: F, deliver: D) -> JoinHandle<()>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
        D: FnOnce(F::Output) + Send + 'static,
    {
        self.handle.spawn(async move { deliver(future.await) })
    }

    /// Run the future `make` builds, where the future may hold values that
    /// stay on one thread (a browser page's storage handles, where that is
    /// the target): natively it is built and driven on one thread of the
    /// blocking pool, with the runtime's drivers.
    pub fn spawn_pinned<M, F>(&self, make: M) -> JoinHandle<F::Output>
    where
        M: FnOnce() -> F + Send + 'static,
        F: Future + 'static,
        F::Output: Send + 'static,
    {
        let handle = self.handle.clone();
        self.handle.spawn_blocking(move || {
            let _driving = crate::Driving::enter();
            handle.block_on(make())
        })
    }

    /// Run a blocking call on the runtime's bounded blocking pool.
    pub fn spawn_blocking<F, R>(&self, call: F) -> JoinHandle<R>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        self.handle.spawn_blocking(call)
    }

    /// Wait on `future` from a synchronous thread.
    ///
    /// For threads that own a blocking API (a git transport callback, a
    /// decoder loop, a synchronous library entry point) and need one async
    /// result. The caller polls `future` itself — so it may borrow — while
    /// the runtime thread drives its sockets and timers and runs anything it
    /// spawns. No runtime is built per call.
    ///
    /// # Errors
    ///
    /// [`Error::UiThread`] on a UI thread, [`Error::InsideRuntime`] from a
    /// runtime context of this crate's, where waiting would stall the tasks
    /// that thread is meant to run.
    ///
    /// A thread inside another runtime's context (a library's or a test's
    /// own runtime) also polls `future` here, while this runtime's thread
    /// drives the sockets and timers: tokio's own `block_on` would panic
    /// there, and this never hands the thread to a second runtime. Such a
    /// caller starts no second runtime and cannot await: it keeps its own
    /// runtime's tasks from running for the length of the wait, so the future
    /// it waits on must need nothing but this runtime.
    pub fn block_on<F: Future>(&self, future: F) -> Result<F::Output, Error> {
        crate::check_not_a_driver()?;
        let _driving = crate::Driving::enter();
        if Handle::try_current().is_ok() {
            let _app = self.handle.enter();
            return Ok(park_on(future));
        }
        Ok(self.handle.block_on(future))
    }
}

fn park_on<F: Future>(future: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread::Thread;

    struct Unpark(Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::park();
    }
}
