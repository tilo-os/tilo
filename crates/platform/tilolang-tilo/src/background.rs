//! Work that reads a whole workspace off the frame: a package's index built from the record, and
//! publication's first trees read from a [`crate::store::Snapshot`], on the worker threads the
//! process keeps; a frame after their end takes the result in.

use std::any::Any;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

/// What a host passes to have a frame scheduled when background work ends: the window's
/// redraw request. Called from the worker thread.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// How many threads read off the frame at once.
const WORKERS: usize = 2;

/// How often work whose worker ended without a result is queued again before the frame it is
/// owed to sees the panic.
const RESUMES: u32 = 1;

/// How long a wait blocks before it starts the workers a process at its thread limit could not
/// start when the work was queued.
const RETRY: Duration = Duration::from_millis(100);

/// The work of one [`Pending`], run again when its worker ended without a result.
type Work<T> = Arc<Mutex<Box<dyn Fn() -> T + Send>>>;

type Job = Box<dyn FnOnce() + Send>;

/// The threads that read off the frame, and the jobs queued for them.
struct Workers {
    queue: Mutex<VecDeque<Job>>,
    queued: Condvar,
    /// The workers started so far: one the process could not start is one the next job starts.
    started: AtomicUsize,
}

fn workers() -> &'static Workers {
    static POOL: OnceLock<Workers> = OnceLock::new();
    POOL.get_or_init(|| Workers {
        queue: Mutex::new(VecDeque::new()),
        queued: Condvar::new(),
        started: AtomicUsize::new(0),
    })
}

fn lock<T>(held: &Mutex<T>) -> MutexGuard<'_, T> {
    held.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Queue `job` for a worker, starting the workers not running yet.
fn submit(job: Job) {
    let workers = workers();
    // The queue lives as long as the process: what is queued is taken by a worker, never lost.
    lock(&workers.queue).push_back(job);
    workers.queued.notify_one();
    start_workers();
}

/// The next job queued, waiting for one.
fn next_job(workers: &Workers) -> Job {
    let mut queue = lock(&workers.queue);
    loop {
        if let Some(job) = queue.pop_front() {
            drop(queue);
            return job;
        }
        queue = workers
            .queued
            .wait(queue)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

/// Start the workers not running yet: one the process could not start is started by the next job
/// queued, or by the frame that waits for one.
fn start_workers() {
    let workers = workers();
    while workers.started.load(Ordering::Acquire) < WORKERS {
        let spawned = std::thread::Builder::new()
            .name("tilolang-read".to_owned())
            .spawn(|| {
                crate::index::read_off_frame();
                loop {
                    let job = next_job(workers);
                    // The job answers the frame that queued it: what it panics with after the
                    // work ended ends no worker the process's other reads wait on.
                    let _ = catch_unwind(AssertUnwindSafe(job));
                }
            });
        if spawned.is_err() {
            return;
        }
        workers.started.fetch_add(1, Ordering::AcqRel);
    }
}

/// What a worker last said of its work.
enum Answer<T> {
    Reading,
    Ready(T),
    /// The work panicked, with what it panicked with.
    Panicked(Box<dyn Any + Send>),
}

/// The answer of a job, and the frame waiting for it.
struct Slot<T> {
    answer: Mutex<Answer<T>>,
    ready: Condvar,
}

/// Work read off the frame on a worker thread.
pub(crate) struct Pending<T> {
    slot: Arc<Slot<T>>,
    work: Work<T>,
    wake: Option<Waker>,
    /// The content store the work reads through, as the window registered it on the frame.
    content: Option<tilo_content::ContentStore>,
    resumes: u32,
}

impl<T: Send + 'static> Pending<T> {
    /// Read `work` off the frame, then call `wake`. A busy process starts no thread for it: the
    /// work joins the queue of the worker threads the process keeps.
    pub fn start(work: impl Fn() -> T + Send + 'static, wake: Option<Waker>) -> Self {
        let pending = Self {
            slot: Arc::new(Slot {
                answer: Mutex::new(Answer::Reading),
                ready: Condvar::new(),
            }),
            work: Arc::new(Mutex::new(Box::new(work))),
            wake,
            // The work reads what the window's record keeps as content (an action too large to
            // record whole) through the store the window registered, as the window's code does.
            content: tilo_actions::content::current(),
            resumes: 0,
        };
        pending.queue();
        pending
    }

    /// Queue the work for a worker.
    fn queue(&self) {
        let slot = Arc::clone(&self.slot);
        let work = Arc::clone(&self.work);
        let wake = self.wake.clone();
        let content = self.content.clone();
        submit(Box::new(move || {
            let scope = content.map(|store| {
                let scope = tilo_wake::WakeScope::new();
                tilo_actions::content::register(&scope.handle(), store);
                scope
            });
            let _in_the_window = scope.as_ref().map(tilo_wake::WakeScope::enter);
            let read = catch_unwind(AssertUnwindSafe(|| {
                let work = lock(&work);
                work()
            }));
            let mut answer = lock(&slot.answer);
            *answer = match read {
                Ok(value) => Answer::Ready(value),
                Err(payload) => Answer::Panicked(payload),
            };
            drop(answer);
            slot.ready.notify_all();
            if let Some(wake) = wake {
                wake();
            }
        }));
    }

    /// The result, if the work has ended. Work whose worker ended without one is queued again;
    /// one that ends so twice panics here, on the frame that is owed it.
    pub fn poll(&mut self) -> Option<T> {
        let slot = Arc::clone(&self.slot);
        let mut answer = lock(&slot.answer);
        match std::mem::replace(&mut *answer, Answer::Reading) {
            Answer::Reading => {
                drop(answer);
                start_workers();
                None
            }
            Answer::Ready(value) => Some(value),
            Answer::Panicked(payload) => {
                self.lost(payload, answer);
                None
            }
        }
    }

    /// Wait for the work to end.
    pub fn wait(&mut self) -> T {
        let slot = Arc::clone(&self.slot);
        let mut answer = lock(&slot.answer);
        loop {
            match std::mem::replace(&mut *answer, Answer::Reading) {
                Answer::Reading => {
                    start_workers();
                    answer = slot
                        .ready
                        .wait_timeout(answer, RETRY)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                Answer::Ready(value) => return value,
                Answer::Panicked(payload) => {
                    self.lost(payload, answer);
                    answer = lock(&slot.answer);
                }
            }
        }
    }

    /// The worker ended without a result: queue the work again, or panic with what it panicked
    /// with once it ends so again.
    fn lost(&mut self, payload: Box<dyn Any + Send>, answer: MutexGuard<'_, Answer<T>>) {
        if self.resumes >= RESUMES {
            drop(answer);
            resume_unwind(payload);
        }
        self.resumes = self.resumes.saturating_add(1);
        drop(answer);
        self.queue();
    }
}
