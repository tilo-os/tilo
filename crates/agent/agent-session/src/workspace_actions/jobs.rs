//! Where the service's requests go: each workspace's to the one thread that
//! serves it.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

use super::{CatalogueFactory, Job, Shared, Worker, WorkspaceDocumentsLink};

impl Job {
    /// The workspace whose thread serves the job: every job is about one.
    const fn served_in(&self) -> WorkspaceId {
        match self {
            Self::Feedback { workspace_id, .. }
            | Self::Catalogue { workspace_id, .. }
            | Self::Invoke { workspace_id, .. }
            | Self::Content { workspace_id, .. }
            | Self::ContentGet { workspace_id, .. }
            | Self::ContentLink { workspace_id, .. }
            | Self::ReadDocument { workspace_id, .. }
            | Self::Search { workspace_id, .. }
            | Self::Changes { workspace_id, .. }
            | Self::Invocations { workspace_id, .. }
            | Self::Changed { workspace_id, .. }
            | Self::Arrived { workspace_id, .. }
            | Self::Joined(workspace_id)
            | Self::Called(workspace_id)
            | Self::Loaded(workspace_id)
            | Self::Forget(workspace_id)
            | Self::Rename(workspace_id, _)
            | Self::Landed(workspace_id, _) => *workspace_id,
            Self::Page(job) => job.workspace(),
            Self::Ran(ended) | Self::Typed(ended) => ended.workspace_id,
            Self::Opened(ended) => ended.workspace_id,
        }
    }
}

/// Where the service's requests go: each workspace's to the one thread that
/// serves it, so a workspace's requests run in the order they came while
/// other workspaces' run beside them.
#[derive(Clone)]
pub(super) struct Jobs {
    inner: Arc<Inner>,
}

/// A request waiting for a thread, and when it came.
type Queued = (Instant, Job);

/// One thread's queue of requests.
type Queue = mpsc::Sender<Queued>;

struct Inner {
    /// Each thread's queue. Empty once the service stopped: every thread then
    /// sees its queue close, serves what waits and ends.
    shards: Mutex<Arc<[Queue]>>,
    /// The threads the queues are served on, taken to join them once stopped.
    threads: Mutex<Option<Vec<std::thread::JoinHandle<()>>>>,
}

impl Jobs {
    /// Start the service's threads, one for each worker of the app worker
    /// pool ([`crate::worker::pool_size`]), each serving its share of the
    /// workspaces with its records held in a worker of its own.
    ///
    /// # Errors
    ///
    /// A thread did not start.
    pub(super) fn start(
        shared: &Arc<Shared>,
        catalogue: &CatalogueFactory,
        link: &Arc<dyn WorkspaceDocumentsLink>,
    ) -> Result<Self, String> {
        let mut shards = Vec::new();
        let mut threads = Vec::new();
        for index in 0..crate::worker::pool_size() {
            let (sender, jobs) = mpsc::channel();
            let shared = Arc::clone(shared);
            let catalogue = Arc::clone(catalogue);
            let link = Arc::clone(link);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("workspace-actions-{index}"))
                    .spawn(move || {
                        crate::worker::set_affinity(index);
                        Worker::new(link.as_ref(), catalogue(), shared).run(&jobs);
                    })
                    .map_err(|error| error.to_string())?,
            );
            shards.push(sender);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                shards: Mutex::new(shards.into()),
                threads: Mutex::new(Some(threads)),
            }),
        })
    }

    /// Hand `job` to the thread serving its workspace.
    ///
    /// # Errors
    ///
    /// The job back, when that thread has stopped.
    #[allow(
        clippy::result_large_err,
        reason = "a job the thread cannot take goes back to its sender whole"
    )]
    pub(super) fn send(&self, job: Queued) -> Result<(), mpsc::SendError<Queued>> {
        let shards = self.inner.shards.lock_recover();
        let index = shard_of(job.1.served_in(), shards.len());
        match shards.get(index) {
            Some(shard) => shard.send(job),
            None => Err(mpsc::SendError(job)),
        }
    }

    /// Stop the service's threads: each serves what waits and ends, and this
    /// returns once they have.
    ///
    /// Every queue is closed before the join, so a thread in the middle of a
    /// request finishes it and the request behind it finds no queue: no
    /// thread starts one on a runtime that is stopping. The caller must
    /// still be on the runtime those threads work on, since a thread waiting
    /// for it never ends for this to return.
    pub(super) fn stop(&self) {
        let shards = std::mem::replace(
            &mut *self.inner.shards.lock_recover(),
            Arc::from(Vec::<Queue>::new()),
        );
        // Every sender the service started goes here, and copies kept by the
        // commands it runs find none: each thread sees its queue close.
        drop(shards);
        let threads = self.inner.threads.lock_recover().take();
        let join = move || {
            for thread in threads.into_iter().flatten() {
                let _ = thread.join();
            }
        };
        match tokio::runtime::Handle::try_current() {
            // A thread of the runtime is handed off, so the runtime's other
            // threads can work what the threads being joined wait on.
            Ok(runtime)
                if runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread =>
            {
                tokio::task::block_in_place(join);
            }
            // Off any runtime this thread is the join, and a runtime of one
            // thread has no other to keep working.
            _ => join(),
        }
    }
}

/// Which of the service's threads serves `workspace_id`: one of
/// [`crate::worker::pool_size`], the same for every request of it.
#[must_use]
pub fn thread_of(workspace_id: WorkspaceId) -> usize {
    shard_of(workspace_id, crate::worker::pool_size())
}

/// Which of `count` threads serves `workspace_id`.
pub(super) fn shard_of(workspace_id: WorkspaceId, count: usize) -> usize {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    workspace_id.hash(&mut hasher);
    let count = u64::try_from(count).unwrap_or(1).max(1);
    usize::try_from(hasher.finish().checked_rem(count).unwrap_or_default()).unwrap_or_default()
}
