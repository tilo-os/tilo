//! The workers a record's writes run on, one per record: the record an action leaves is the one
//! its next write starts from, and that is kept per thread, so a record's writes must always run
//! on the same one. Each record is given a worker by a stable hash of its address, and its writes
//! are done there.
//!
//! A write of a record is already one at a time within the workspace's hold, so this changes no
//! ordering: it only says which thread does the work, and so which thread keeps the record.
//! The write itself is sent as a job and its answer awaited, so the calling task waits on its
//! worker without blocking one of the runtime's own threads.

use std::sync::mpsc::{SendError, Sender};
use std::sync::{LazyLock, Mutex, PoisonError};

/// How many workers keep records. A record's writes are a small share of what a device does, and
/// a worker keeps a handful of records; four spreads the writes a device makes at once without
/// keeping a record for every one.
const WORKERS: usize = 4;

/// A job run on a record's worker, with its answer sent back.
type Job = Box<dyn FnOnce() + Send + 'static>;

thread_local! {
    /// Whether this thread is one of the pool's, so a job that writes another record runs it
    /// here rather than waiting on itself.
    static ON_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct Worker {
    jobs: Mutex<Sender<Job>>,
}

static POOL: LazyLock<Vec<Worker>> = LazyLock::new(|| {
    (0..WORKERS)
        .filter_map(|_| {
            let (jobs, work) = std::sync::mpsc::channel::<Job>();
            std::thread::Builder::new()
                .name("record-writer".to_owned())
                .spawn(move || {
                    ON_WORKER.with(|on| on.set(true));
                    while let Ok(job) = work.recv() {
                        job();
                    }
                })
                .ok()?;
            Some(Worker {
                jobs: Mutex::new(jobs),
            })
        })
        .collect()
});

/// The worker that does `record`'s writes: one of the pool, chosen the same way on every device
/// for the same record, or `None` when no worker started.
fn worker_for(record: &str) -> Option<&'static Worker> {
    let workers = POOL.as_slice();
    if workers.is_empty() {
        return None;
    }
    // FNV-1a: a stable hash across devices, so a record's writes go to the same worker on each.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in record.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let divisor = u64::try_from(workers.len()).unwrap_or(1);
    let at = usize::try_from(hash.checked_rem(divisor).unwrap_or(0)).unwrap_or(0);
    workers.get(at)
}

/// Run `job` on the worker that does `record`'s writes, and answer what it answered. A worker
/// that stopped takes the job here instead; a job that panics is raised on this thread, as it
/// would have been run here.
pub async fn run<R: Send + 'static>(
    record: &str,
    job: impl FnOnce() -> R + Send + 'static,
) -> Result<R, String> {
    // A record written from inside a job is written on the worker already running: waiting on
    // this thread for another job of it would wait for itself.
    if ON_WORKER.with(std::cell::Cell::get) {
        return Ok(job());
    }
    let (answer, waiting) = tokio::sync::oneshot::channel();
    let job: Job = Box::new(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        let _ = answer.send(outcome);
    });
    let sent = match worker_for(record) {
        Some(worker) => worker
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(job),
        None => Err(SendError(job)),
    };
    // A worker that stopped after it was taken takes the job here.
    if let Err(SendError(job)) = sent {
        job();
    }
    match waiting.await {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => Err("the record's writer stopped".to_owned()),
    }
}
