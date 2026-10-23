//! Where a workspace keeps its compute routing, and the store's records of
//! its jobs.

use super::{ComputeTransportState, DurableComputeJob};
use crate::store::{StoreError, WorkspaceStore};

/// `runtime`'s compute routing, made the first time the compute plane asks
/// for it. Making it ends the jobs an earlier run of this relay left
/// unfinished: nothing in this process runs one yet, since no compute
/// session is attached before this, and a worker's jobs end with its
/// session to the relay. A failure leaves the jobs as they were.
pub(crate) fn compute_state(
    runtime: &mut crate::workspace::WorkspaceRuntime,
) -> &mut ComputeTransportState {
    compute_parts(runtime).0
}

/// `runtime`'s compute routing, as [`compute_state`] makes it, and the store
/// its jobs are kept in.
pub(crate) fn compute_parts(
    runtime: &mut crate::workspace::WorkspaceRuntime,
) -> (&mut ComputeTransportState, &mut WorkspaceStore) {
    let crate::workspace::WorkspaceRuntime {
        extensions, store, ..
    } = runtime;
    let state = extensions.get_or_insert_with(|| {
        let mut state = ComputeTransportState::default();
        let workspace_id = store.workspace_id();
        match state.fail_unfinished_jobs(store, &mut crate::compute_session::next_queue_sequence) {
            Ok(0) => {}
            Ok(ended) => tracing::info!(
                workspace_id,
                ended,
                "ended the compute jobs a relay restart left unfinished"
            ),
            Err(error) => tracing::warn!(
                workspace_id,
                %error,
                "could not end the compute jobs a relay restart left unfinished"
            ),
        }
        state
    });
    (state, store)
}

/// A workspace store's compute job records, read and written as
/// [`DurableComputeJob`]s.
pub trait ComputeJobs {
    /// [`WorkspaceStore::claim_compute_record`] for `record`'s job.
    ///
    /// # Errors
    ///
    /// As [`WorkspaceStore::claim_compute_record`].
    fn claim_compute_job(
        &self,
        record: &DurableComputeJob,
    ) -> Result<Option<DurableComputeJob>, StoreError>;
    /// [`WorkspaceStore::load_compute_record`].
    ///
    /// # Errors
    ///
    /// As [`WorkspaceStore::load_compute_record`].
    fn load_compute_job(&self, job_id: &[u8; 16]) -> Result<Option<DurableComputeJob>, StoreError>;
    /// [`WorkspaceStore::save_compute_record`] for `record`'s job.
    ///
    /// # Errors
    ///
    /// As [`WorkspaceStore::save_compute_record`].
    fn save_compute_job(&self, record: &DurableComputeJob) -> Result<(), StoreError>;
    /// [`WorkspaceStore::load_compute_records`].
    ///
    /// # Errors
    ///
    /// As [`WorkspaceStore::load_compute_records`].
    fn load_compute_jobs(&self) -> Result<Vec<DurableComputeJob>, StoreError>;
}

impl ComputeJobs for WorkspaceStore {
    fn claim_compute_job(
        &self,
        record: &DurableComputeJob,
    ) -> Result<Option<DurableComputeJob>, StoreError> {
        self.claim_compute_record(&record.spec.job_id, record)
    }

    fn load_compute_job(&self, job_id: &[u8; 16]) -> Result<Option<DurableComputeJob>, StoreError> {
        self.load_compute_record(job_id)
    }

    fn save_compute_job(&self, record: &DurableComputeJob) -> Result<(), StoreError> {
        self.save_compute_record(&record.spec.job_id, record)
    }

    fn load_compute_jobs(&self) -> Result<Vec<DurableComputeJob>, StoreError> {
        self.load_compute_records()
    }
}
