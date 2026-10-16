//! Hydration behind history restores and conflict resolutions.
//!
//! Restoring a version or resolving a conflict can need content no peer has
//! sent this device yet — gigabytes of it, for an old version of a large
//! file. Fetching it inside the FUSE `setxattr` callback parks one of a
//! handful of FUSE workers for the whole download, so every other process
//! using the mount waits behind it, and nobody can see progress or stop it.
//!
//! Instead the callback asks [`HistoryJobs::ensure_local`]. When every byte
//! is already local the operation commits at once, as before. Otherwise a job
//! hydrates the content on the daemon runtime — into the content store, never
//! into one in-memory buffer — and the callback answers `EINPROGRESS`. The
//! caller reads the job's progress, may cancel it, and repeats its exact
//! request once the job is ready. That repeat runs every authorization,
//! freshness and pending-write check again before it commits, so nothing
//! observed before the download is trusted after it.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use fs_protocol::ContentHash;
use fs_protocol::files::{HistoryJobResponseV1, HistoryJobStateV1 as State};
use iroh_blobs::Hash;

use super::{Access, QuickFS};
use crate::hydration::{Hydrator, Priority};
use tilo_lock::MutexRecover as _;

/// Jobs one mount keeps at once. Finished jobs nobody came back for are
/// dropped first; past that a new request is refused rather than queued.
pub(super) const MAX_HISTORY_JOBS: usize = 32;

/// One job: the restore intent or the conflict being resolved, on the inode
/// it was requested through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct JobKey {
    pub ino: u64,
    pub id: uuid::Uuid,
}

/// Whether a history operation may commit now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Readiness {
    /// Every byte is local: commit.
    Local,
    /// A job is downloading what is missing.
    Hydrating,
    /// The job could not hydrate the content. Reported once; the next
    /// request starts a new job.
    Failed,
    /// The job was cancelled. Reported once, like [`Self::Failed`].
    Cancelled,
    /// Too many jobs are downloading already.
    Busy,
    /// The id names a job for different content.
    Mismatch,
}

struct Job {
    hashes: Vec<Hash>,
    state: Arc<Mutex<State>>,
    task: tokio::task::AbortHandle,
}

impl Job {
    fn state(&self) -> State {
        *self.state.lock_recover()
    }
}

/// The hydration jobs of one mount.
#[derive(Default)]
pub(super) struct HistoryJobs {
    jobs: Mutex<HashMap<JobKey, Job>>,
    /// Test-only: jobs wait for a permit before they fetch anything.
    #[cfg(test)]
    hold: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
}

impl HistoryJobs {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<JobKey, Job>> {
        self.jobs.lock_recover()
    }

    /// Whether the content behind `hashes` is local, starting a job to make
    /// it so when it is not. Never waits on the network: the only wait is
    /// the local residency check.
    pub(super) fn ensure_local(
        &self,
        key: JobKey,
        hydrator: &Arc<Hydrator>,
        hashes: &[Hash],
    ) -> Readiness {
        let mut unique = Vec::with_capacity(hashes.len());
        for hash in hashes {
            if !unique.contains(hash) {
                unique.push(*hash);
            }
        }
        {
            let mut jobs = self.lock();
            if let Some(job) = jobs.get(&key) {
                let same = job.hashes == unique;
                match (same, job.state()) {
                    (true, State::Hydrating { .. }) => return Readiness::Hydrating,
                    (false, State::Hydrating { .. }) => return Readiness::Mismatch,
                    (true, State::Failed) => {
                        jobs.remove(&key);
                        return Readiness::Failed;
                    }
                    (true, State::Cancelled) => {
                        jobs.remove(&key);
                        return Readiness::Cancelled;
                    }
                    // Ready is consumed by this request; if the content has
                    // been evicted since, the residency check below starts
                    // over. A finished job for other content — the current
                    // version moved on meanwhile — answers nothing now.
                    (true, State::Ready) | (false, _) => {
                        jobs.remove(&key);
                    }
                }
            }
        }
        if hydrator.block_on(all_local(hydrator, &unique)) {
            return Readiness::Local;
        }
        let mut jobs = self.lock();
        if let Some(job) = jobs.get(&key) {
            // A concurrent request for the same id started it meanwhile.
            return if job.hashes == unique {
                Readiness::Hydrating
            } else {
                Readiness::Mismatch
            };
        }
        if jobs.len() >= MAX_HISTORY_JOBS {
            jobs.retain(|_, job| matches!(job.state(), State::Hydrating { .. }));
        }
        if jobs.len() >= MAX_HISTORY_JOBS {
            return Readiness::Busy;
        }
        let state = Arc::new(Mutex::new(State::Hydrating { done: 0, total: 0 }));
        let job = {
            let hydrator = Arc::clone(hydrator);
            let hashes = unique.clone();
            let state = Arc::clone(&state);
            #[cfg(test)]
            let hold = self.hold.lock_recover().clone();
            async move {
                #[cfg(test)]
                if let Some(hold) = hold {
                    let _ = hold.acquire().await;
                }
                let hydrated = hydrate(&hydrator, &hashes, |next| advance(&state, next)).await;
                advance(
                    &state,
                    if hydrated {
                        State::Ready
                    } else {
                        State::Failed
                    },
                );
            }
        };
        let task = hydrator.runtime_handle.spawn(job).abort_handle();
        jobs.insert(
            key,
            Job {
                hashes: unique,
                state,
                task,
            },
        );
        Readiness::Hydrating
    }

    /// Where the job stands, if there is one.
    pub(super) fn state(&self, key: JobKey) -> Option<State> {
        self.lock().get(&key).map(Job::state)
    }

    /// Stop a downloading job. `false` when there is none, or it already
    /// finished. The content it already fetched stays in the store, subject
    /// to normal eviction.
    pub(super) fn cancel(&self, key: JobKey) -> bool {
        let Some((state, task)) = self
            .lock()
            .get(&key)
            .map(|job| (Arc::clone(&job.state), job.task.clone()))
        else {
            return false;
        };
        {
            let mut state = state.lock_recover();
            if !matches!(*state, State::Hydrating { .. }) {
                return false;
            }
            *state = State::Cancelled;
        }
        task.abort();
        true
    }
}

#[cfg(test)]
impl HistoryJobs {
    /// Hold every job started from now on before it fetches anything, until
    /// the returned semaphore gets a permit.
    pub(super) fn hold(&self) -> Arc<tokio::sync::Semaphore> {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *self.hold.lock_recover() = Some(Arc::clone(&gate));
        gate
    }
}

impl Drop for HistoryJobs {
    fn drop(&mut self) {
        for job in self.lock().values() {
            job.task.abort();
        }
    }
}

async fn all_local(hydrator: &Hydrator, hashes: &[Hash]) -> bool {
    for hash in hashes {
        if hydrator.local_len(*hash).await.is_none() {
            return false;
        }
    }
    true
}

/// Set a downloading job's state; a cancelled job keeps its state.
fn advance(state: &Mutex<State>, next: State) {
    let mut state = state.lock_recover();
    if matches!(*state, State::Hydrating { .. }) {
        *state = next;
    }
}

/// Make every byte of `hashes` local, reporting verified progress across
/// all of them. `false` when a manifest or chunk could not be fetched.
async fn hydrate(
    hydrator: &Hydrator,
    hashes: &[Hash],
    mut report: impl FnMut(State) + Send,
) -> bool {
    let result = async {
        let mut total = 0_u64;
        for hash in hashes {
            total = total.saturating_add(hydrator.content_len(*hash).await?);
        }
        let mut base = 0_u64;
        for hash in hashes {
            let mut len = 0_u64;
            hydrator
                .prefetch_with_progress(*hash, u64::MAX, Priority::Foreground, |done, of| {
                    len = of;
                    report(State::Hydrating {
                        done: base.saturating_add(done),
                        total,
                    });
                })
                .await?;
            base = base.saturating_add(len);
        }
        anyhow::Ok(())
    }
    .await;
    result.is_ok()
}

impl QuickFS {
    /// Whether the immutable versions behind a history operation are local,
    /// as the operation's errno: `EINPROGRESS` while a job downloads them.
    pub(super) fn history_content_local(
        &self,
        ino: u64,
        id: uuid::Uuid,
        versions: &[Option<ContentHash>],
    ) -> Result<(), i32> {
        let hashes: Vec<Hash> = versions.iter().flatten().map(|h| Hash::from(h.0)).collect();
        match self
            .history_jobs
            .ensure_local(JobKey { ino, id }, &self.hydrator, &hashes)
        {
            Readiness::Local => Ok(()),
            Readiness::Hydrating => Err(libc::EINPROGRESS),
            Readiness::Failed => Err(libc::EIO),
            Readiness::Cancelled => Err(libc::ECANCELED),
            Readiness::Busy => Err(libc::EAGAIN),
            Readiness::Mismatch => Err(libc::EBUSY),
        }
    }

    /// The byte length of an immutable version whose content is local;
    /// `None` when any of it is not. A version without content is empty.
    pub(super) fn local_version_len(&self, version: Option<ContentHash>) -> Option<u64> {
        match version {
            Some(hash) => self
                .hydrator
                .block_on(self.hydrator.local_len(Hash::from(hash.0))),
            None => Some(0),
        }
    }

    /// The bytes of an immutable version whose content is local; `None`
    /// when any of it is not. Never fetches.
    pub(super) fn local_version_bytes(&self, version: Option<ContentHash>) -> Option<Vec<u8>> {
        match version {
            Some(hash) => {
                self.hydrator
                    .block_on(self.hydrator.read_local(Hash::from(hash.0), 0, u64::MAX))
            }
            None => Some(Vec::new()),
        }
    }

    /// Where the hydration behind history request `id` on `ino` stands.
    pub fn history_job(&self, ino: u64, id: uuid::Uuid) -> Option<State> {
        self.history_jobs.state(JobKey { ino, id })
    }

    /// Cancel the hydration behind history request `id` on `ino`.
    ///
    /// # Errors
    ///
    /// The caller may not write the file, or no download is running.
    pub fn cancel_history_job(&self, ino: u64, id: uuid::Uuid) -> Result<(), i32> {
        self.check_access(ino, Access::Write)?;
        if self.history_jobs.cancel(JobKey { ino, id }) {
            Ok(())
        } else {
            Err(libc::ENOENT)
        }
    }

    /// The `user.tilo.history-job.<id>` reply.
    pub(super) fn history_job_xattr(&self, ino: u64, id: uuid::Uuid) -> Result<Vec<u8>, i32> {
        self.check_access(ino, Access::Read)?;
        let state = self.history_job(ino, id).ok_or(libc::ENODATA)?;
        serde_json::to_vec(&HistoryJobResponseV1 {
            schema_version: 1,
            state,
        })
        .map_err(|_| libc::EIO)
    }
}

#[cfg(test)]
#[path = "history_jobs/tests.rs"]
mod tests;
