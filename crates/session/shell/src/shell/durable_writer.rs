//! Durable session state written off the frame thread.
//!
//! Saving the workspace tree replaces three files and fsyncs each one and
//! its directory. On a virtual disk that is tens of milliseconds, which a
//! keypress that switches workspaces must not wait for. Callers take a
//! snapshot of what they want on disk while they still hold the state, and
//! hand the write to this module's thread:
//!
//! - **Latest wins.** Writes are keyed by the file they produce. A newer
//!   write for a file that has not started yet replaces the queued one, so a
//!   burst of mutations costs one or two writes, never one per mutation.
//! - **Ordered.** Queued writes run one at a time, oldest first; a write that
//!   replaced an older one keeps the older one's place in line.
//! - **Bounded delay.** Nothing waits for a quiet period: a write starts as
//!   soon as the one before it finishes.
//! - **Barriers.** [`DurableWriter::flush`] returns once everything submitted
//!   before it is on disk. Session exit, logout, suspend and any in-process
//!   reader of the files call it.
//!
//! Crash safety is the job's own: every job writes through an atomic
//! replace, so an interrupted write leaves the previous file intact.

use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

type Job = Box<dyn FnOnce() + Send>;

#[derive(Clone, PartialEq, Eq, Hash)]
enum WriteKey {
    Snapshot(PathBuf),
    Ordered(u64),
}

struct Queued {
    /// The oldest submission this entry stands for. A replacement keeps it,
    /// so a barrier taken before the first submission waits for the newest.
    ticket: u64,
    job: Job,
}

#[derive(Default)]
struct Queue {
    queued: HashMap<WriteKey, Queued>,
    /// Ticket of the write in progress.
    running: Option<u64>,
    submitted: u64,
}

impl Queue {
    /// Every ticket up to and including this one is on disk.
    fn durable_through(&self) -> u64 {
        self.queued
            .values()
            .map(|queued| queued.ticket)
            .chain(self.running)
            .min()
            .map_or(self.submitted, |oldest| oldest.saturating_sub(1))
    }

    fn take_oldest(&mut self) -> Option<(u64, Job)> {
        let key = self
            .queued
            .iter()
            .min_by_key(|(_, queued)| queued.ticket)
            .map(|(key, _)| key.clone())?;
        let queued = self.queued.remove(&key)?;
        Some((queued.ticket, queued.job))
    }
}

struct Shared {
    queue: Mutex<Queue>,
    work: Condvar,
    /// Signalled when a write finishes.
    done: Condvar,
}

#[cfg(test)]
thread_local! {
    /// Writes this thread handed to [`DurableWriter::submit`], so a test can
    /// tell a call that queued a write from one that found nothing to do.
    pub(crate) static SUBMITS_ON_THIS_THREAD: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// A single background thread that performs queued state writes.
pub struct DurableWriter {
    shared: Arc<Shared>,
    /// `None` when the thread could not be started; writes then happen on
    /// the caller's thread, as they would without this module.
    thread: Option<std::thread::ThreadId>,
}

impl DurableWriter {
    /// Start a writer thread. It runs for the life of the process.
    #[must_use]
    pub fn spawn(name: &str) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            work: Condvar::new(),
            done: Condvar::new(),
        });
        let worker = shared.clone();
        let thread = match std::thread::Builder::new()
            .name(name.to_string())
            .spawn(move || run(&worker))
        {
            Ok(handle) => Some(handle.thread().id()),
            Err(error) => {
                tracing::error!(%error, "could not start the state writer; saving on the caller's thread");
                None
            }
        };
        Self { shared, thread }
    }

    /// Queue `job`, the write that produces `key`, replacing any write for
    /// `key` that has not started.
    pub fn submit(&self, key: PathBuf, job: impl FnOnce() + Send + 'static) {
        #[cfg(test)]
        SUBMITS_ON_THIS_THREAD.with(|submits| submits.set(submits.get().saturating_add(1)));
        if self.thread.is_none() {
            job();
            return;
        }
        let mut queue = self.shared.queue.lock();
        queue.submitted = queue.submitted.saturating_add(1);
        let ticket = queue.submitted;
        let job: Job = Box::new(job);
        let replaced = match queue.queued.entry(WriteKey::Snapshot(key)) {
            std::collections::hash_map::Entry::Occupied(mut queued) => {
                Some(std::mem::replace(&mut queued.get_mut().job, job))
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(Queued { ticket, job });
                None
            }
        };
        drop(queue);
        self.shared.work.notify_one();
        // Dropped with the queue let go: what the replaced write owned may
        // queue a write of its own as it goes.
        drop(replaced);
    }

    /// Queue a command whose completion must not be replaced by a later
    /// snapshot. Admission is bounded and never falls back to blocking the
    /// caller when the worker is unavailable.
    ///
    /// # Errors
    /// Returns an error if the worker is unavailable or 32 commands already
    /// await storage. Existing coalesced snapshot writes keep their contract.
    pub fn submit_ordered(&self, job: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
        if self.thread.is_none() {
            return Err(std::io::Error::other("the state writer is unavailable"));
        }
        let mut queue = self.shared.queue.lock();
        if queue
            .queued
            .keys()
            .filter(|key| matches!(key, WriteKey::Ordered(_)))
            .count()
            >= 32
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "the state writer command queue is full",
            ));
        }
        let ticket = queue
            .submitted
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("state writer tickets exhausted"))?;
        queue.submitted = ticket;
        queue.queued.insert(
            WriteKey::Ordered(ticket),
            Queued {
                ticket,
                job: Box::new(job),
            },
        );
        drop(queue);
        self.shared.work.notify_one();
        Ok(())
    }

    /// Wait until every write submitted before this call is on disk.
    pub fn flush(&self) {
        self.flush_within(None);
    }

    /// [`Self::flush`], giving up after `limit`. Returns whether everything
    /// was written. A barrier on the writer's own thread returns at once: the
    /// write it would wait for is the one running it.
    pub fn flush_timeout(&self, limit: Duration) -> bool {
        self.flush_within(Instant::now().checked_add(limit))
    }

    /// How many submitted writes are on disk: it advances whenever the
    /// writer completes one, so two equal readings a while apart mean it
    /// completed nothing in between.
    pub fn progress(&self) -> u64 {
        self.shared.queue.lock().durable_through()
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the queue lock is what the condition variable waits on"
    )]
    fn flush_within(&self, deadline: Option<Instant>) -> bool {
        if self
            .thread
            .is_none_or(|thread| thread == std::thread::current().id())
        {
            return true;
        }
        let mut queue = self.shared.queue.lock();
        let target = queue.submitted;
        while queue.durable_through() < target {
            match deadline {
                Some(deadline) => {
                    if self
                        .shared
                        .done
                        .wait_until(&mut queue, deadline)
                        .timed_out()
                    {
                        return queue.durable_through() >= target;
                    }
                }
                None => self.shared.done.wait(&mut queue),
            }
        }
        true
    }
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the queue lock is released around each write and is what the condition variable waits on"
)]
fn run(shared: &Shared) {
    let mut queue = shared.queue.lock();
    loop {
        let Some((ticket, job)) = queue.take_oldest() else {
            shared.work.wait(&mut queue);
            continue;
        };
        queue.running = Some(ticket);
        drop(queue);
        // A failing write is the job's to report; one that panics must not
        // take every later write down with it.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
            tracing::error!(target: "shell::persist", "a state write panicked");
        }
        queue = shared.queue.lock();
        queue.running = None;
        shared.done.notify_all();
    }
}

/// The process's state writer.
pub fn global() -> &'static DurableWriter {
    static WRITER: OnceLock<DurableWriter> = OnceLock::new();
    WRITER.get_or_init(|| DurableWriter::spawn("tilo-state-writer"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A writer whose thread is parked inside a job until the returned
    /// sender is used, so the test controls what is queued behind it.
    fn blocked_writer() -> (DurableWriter, mpsc::Sender<()>) {
        let writer = DurableWriter::spawn("durable-writer-test");
        let (release, wait) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<()>();
        writer.submit(PathBuf::from("blocker"), move || {
            started.send(()).expect("test alive");
            let _ = wait.recv();
        });
        running.recv().expect("blocker started");
        (writer, release)
    }

    #[test]
    fn progress_advances_only_as_writes_complete() {
        let (writer, release) = blocked_writer();
        let before = writer.progress();
        writer.submit(PathBuf::from("queued"), || {});
        assert_eq!(writer.progress(), before, "nothing completes while blocked");
        release.send(()).unwrap();
        writer.flush();
        assert!(writer.progress() > before);
    }

    /// A write replaced before it ran is dropped, and what it owned may
    /// queue a write of its own as it goes (a policy export does).
    #[test]
    fn a_replaced_write_may_queue_another_as_it_is_dropped() {
        struct QueuesOnDrop(Arc<DurableWriter>, Arc<Mutex<Vec<&'static str>>>);
        impl Drop for QueuesOnDrop {
            fn drop(&mut self) {
                let written = self.1.clone();
                self.0.submit(PathBuf::from("follow-up"), move || {
                    written.lock().push("follow-up");
                });
            }
        }
        let (writer, release) = blocked_writer();
        let writer = Arc::new(writer);
        let written = Arc::new(Mutex::new(Vec::new()));
        let owned = QueuesOnDrop(writer.clone(), written.clone());
        writer.submit(PathBuf::from("state.json"), move || drop(owned));
        let newest = written.clone();
        writer.submit(PathBuf::from("state.json"), move || {
            newest.lock().push("newest");
        });
        release.send(()).expect("release");
        writer.flush();
        assert_eq!(*written.lock(), vec!["newest", "follow-up"]);
    }

    #[test]
    fn durable_writer_ordered_commands_are_bounded_and_never_coalesced() {
        let (writer, release) = blocked_writer();
        let written = Arc::new(Mutex::new(Vec::new()));
        for index in 0..32 {
            let written = Arc::clone(&written);
            writer
                .submit_ordered(move || written.lock().push(index))
                .unwrap();
        }
        let overflow = writer.submit_ordered(|| panic!("refused job must not run"));
        release.send(()).unwrap();
        writer.flush();
        assert_eq!(overflow.unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(*written.lock(), (0..32).collect::<Vec<_>>());
        let unavailable = DurableWriter {
            shared: Arc::clone(&writer.shared),
            thread: None,
        };
        assert!(
            unavailable
                .submit_ordered(|| panic!("must not block caller"))
                .is_err()
        );
    }

    #[test]
    fn durable_writer_coalesces_orders_and_flushes() {
        {
            let (writer, release) = blocked_writer();
            let written = Arc::new(Mutex::new(Vec::new()));
            for snapshot in 1..=20 {
                let written = written.clone();
                writer.submit(PathBuf::from("state.json"), move || {
                    written.lock().push(snapshot);
                });
            }
            release.send(()).expect("release");
            writer.flush();
            assert_eq!(
                *written.lock(),
                vec![20],
                "twenty saves queued behind one write are one write of the newest"
            );
        }
        {
            let (writer, release) = blocked_writer();
            let written = Arc::new(Mutex::new(Vec::new()));
            for (key, label) in [("a", "a1"), ("b", "b1"), ("a", "a2"), ("c", "c1")] {
                let written = written.clone();
                writer.submit(PathBuf::from(key), move || written.lock().push(label));
            }
            release.send(()).expect("release");
            writer.flush();
            assert_eq!(
                *written.lock(),
                vec!["a2", "b1", "c1"],
                "a replacement keeps its file's place in line and carries the newest content"
            );
        }
        {
            let (writer, release) = blocked_writer();
            let written = Arc::new(Mutex::new(0));
            let first = written.clone();
            writer.submit(PathBuf::from("state.json"), move || *first.lock() = 1);
            let writer = Arc::new(writer);
            let barrier = {
                let writer = writer.clone();
                let written = written.clone();
                std::thread::spawn(move || {
                    writer.flush();
                    *written.lock()
                })
            };
            writer.submit(PathBuf::from("state.json"), move || *written.lock() = 2);
            release.send(()).expect("release");
            assert_eq!(
                barrier.join().expect("barrier"),
                2,
                "a barrier is not satisfied by dropping the write it waited for"
            );
        }
        {
            let (writer, release) = blocked_writer();
            writer.submit(PathBuf::from("state.json"), || {});
            assert!(!writer.flush_timeout(Duration::from_millis(20)));
            release.send(()).expect("release");
            assert!(writer.flush_timeout(tilo_test_env::hang_bound()));
        }
        {
            let writer = DurableWriter::spawn("durable-writer-test");
            writer.submit(PathBuf::from("bad"), || panic!("disk on fire"));
            let written = Arc::new(Mutex::new(false));
            let flag = written.clone();
            writer.submit(PathBuf::from("good"), move || *flag.lock() = true);
            writer.flush();
            assert!(*written.lock());
        }
    }
}
