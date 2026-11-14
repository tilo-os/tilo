//! Where the service's requests go: each workspace's to the one thread that
//! serves it.

use std::sync::{Arc, mpsc};
use std::time::Instant;

use tilo_relay_protocol::WorkspaceId;

use super::{CatalogueFactory, Job, Shared, Worker, WorkspaceRecordsLink};

impl Job {
    /// The workspace whose thread serves the job: every job is about one.
    const fn served_in(&self) -> WorkspaceId {
        match self {
            Self::Feedback { workspace_id, .. }
            | Self::Catalogue { workspace_id, .. }
            | Self::Invoke { workspace_id, .. }
            | Self::Content { workspace_id, .. }
            | Self::ContentGet { workspace_id, .. }
            | Self::Search { workspace_id, .. }
            | Self::Changes { workspace_id, .. }
            | Self::Arrived { workspace_id, .. }
            | Self::Joined(workspace_id)
            | Self::Forget(workspace_id) => *workspace_id,
            Self::Ran(ended) => ended.workspace_id,
        }
    }
}

/// Where the service's requests go: each workspace's to the one thread that
/// serves it, so a workspace's requests run in the order they came while
/// other workspaces' run beside them.
#[derive(Clone)]
pub(super) struct Jobs {
    shards: Arc<[mpsc::Sender<(Instant, Job)>]>,
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
        link: &Arc<dyn WorkspaceRecordsLink>,
    ) -> Result<Self, String> {
        let mut shards = Vec::new();
        for index in 0..crate::worker::pool_size() {
            let (sender, jobs) = mpsc::channel();
            let shared = Arc::clone(shared);
            let catalogue = Arc::clone(catalogue);
            let link = Arc::clone(link);
            std::thread::Builder::new()
                .name(format!("workspace-actions-{index}"))
                .spawn(move || {
                    crate::worker::set_affinity(index);
                    Worker::new(link.as_ref(), catalogue(), shared).run(&jobs);
                })
                .map_err(|error| error.to_string())?;
            shards.push(sender);
        }
        Ok(Self {
            shards: shards.into(),
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
    pub(super) fn send(&self, job: (Instant, Job)) -> Result<(), mpsc::SendError<(Instant, Job)>> {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        job.1.served_in().hash(&mut hasher);
        let count = u64::try_from(self.shards.len()).unwrap_or(1).max(1);
        let index = usize::try_from(hasher.finish().checked_rem(count).unwrap_or_default())
            .unwrap_or_default();
        match self.shards.get(index) {
            Some(shard) => shard.send(job),
            None => Err(mpsc::SendError(job)),
        }
    }
}
