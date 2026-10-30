//! The thread a service's device calls run on, so the session's loop never
//! waits for `NetworkManager` or `PipeWire`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

/// Called whenever something landed, from the worker's thread: what wakes
/// the session's loop to take it in.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// A wake for the session's loop before the loop exists: services are made
/// first and handed [`LoopWake::handle`], and the session binds the loop's
/// waker once it started. Until then a wake does nothing; the loop's first
/// pass takes in whatever landed.
#[derive(Clone, Default)]
pub struct LoopWake(Arc<Mutex<Option<std::task::Waker>>>);

impl LoopWake {
    /// What a service calls when something landed.
    #[must_use]
    pub fn handle(&self) -> Wake {
        let slot = Arc::clone(&self.0);
        Arc::new(move || {
            if let Some(waker) = slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
            {
                waker.wake_by_ref();
            }
        })
    }

    /// Wake `waker`'s loop from now on.
    pub fn bind(&self, waker: std::task::Waker) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(waker);
    }
}

type Job<B, L> = Box<dyn FnOnce(&B) -> L + Send>;

/// The jobs not yet landed, and a way to wait for there being none.
#[derive(Default)]
struct Outstanding {
    count: AtomicUsize,
    lock: Mutex<()>,
    idle: Condvar,
}

impl Outstanding {
    fn started(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }

    fn landed(&self) {
        let _held = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.count.fetch_sub(1, Ordering::SeqCst);
        self.idle.notify_all();
    }

    fn wait(&self) {
        let held = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(
            self.idle
                .wait_while(held, |()| self.count.load(Ordering::SeqCst) > 0)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}

/// Where jobs for a worker are queued from: the service, and anything
/// following the device from another thread (a `NetworkManager` signal
/// asking for a fresh reading).
pub struct Jobs<B: ?Sized, L> {
    sender: Sender<Job<B, L>>,
    outstanding: Arc<Outstanding>,
}

impl<B: ?Sized, L> Clone for Jobs<B, L> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            outstanding: Arc::clone(&self.outstanding),
        }
    }
}

impl<B: ?Sized + 'static, L: 'static> Jobs<B, L> {
    /// Run `job` on the worker's backend; what it returns lands in
    /// [`Worker::landed`].
    pub fn run(&self, job: impl FnOnce(&B) -> L + Send + 'static) {
        self.outstanding.started();
        if self.sender.send(Box::new(job)).is_err() {
            // The worker is gone: nothing will land, so nothing waits.
            self.outstanding.landed();
        }
    }
}

/// One service's device calls, run in order on a thread of its own over
/// the service's backend `B`; each returns an `L` the service takes in.
pub struct Worker<B: ?Sized, L> {
    jobs: Jobs<B, L>,
    landed: Receiver<L>,
}

impl<B: ?Sized + Send + Sync + 'static, L: Send + 'static> Worker<B, L> {
    /// A worker named `name` running jobs over `backend`, calling `wake`
    /// after each landed.
    ///
    /// # Errors
    /// The thread did not start.
    pub fn spawn(name: &str, backend: Arc<B>, wake: Option<Wake>) -> std::io::Result<Self> {
        let (sender, jobs) = mpsc::channel::<Job<B, L>>();
        let (landing, landed) = mpsc::channel();
        let outstanding = Arc::new(Outstanding::default());
        let counted = Arc::clone(&outstanding);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                for job in jobs {
                    let result = job(&backend);
                    let gone = landing.send(result).is_err();
                    counted.landed();
                    if gone {
                        break;
                    }
                    if let Some(wake) = &wake {
                        wake();
                    }
                }
            })?;
        Ok(Self {
            jobs: Jobs {
                sender,
                outstanding,
            },
            landed,
        })
    }
}

impl<B: ?Sized + 'static, L: 'static> Worker<B, L> {
    /// Where to queue jobs from, cloned for other threads.
    #[must_use]
    pub fn jobs(&self) -> Jobs<B, L> {
        self.jobs.clone()
    }

    /// Run `job` on the backend.
    pub fn run(&self, job: impl FnOnce(&B) -> L + Send + 'static) {
        self.jobs.run(job);
    }

    /// What landed since the last call, oldest first.
    pub fn landed(&self) -> Vec<L> {
        self.landed.try_iter().collect()
    }

    /// Wait until every job queued so far landed, then take what landed.
    pub fn settle(&self) -> Vec<L> {
        self.jobs.outstanding.wait();
        self.landed()
    }
}
