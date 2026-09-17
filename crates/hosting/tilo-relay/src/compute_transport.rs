//! Authenticated, durable routing for personal-mesh compute metadata.
//!
//! Package and content bytes never travel in these messages: requests name
//! signed packages and content digests, while results contain only output
//! digests and a tightly bounded diagnostic tail. The relay derives the
//! sender from the certified connection and persists the exact route binding
//! before forwarding anything.

#![allow(
    missing_docs,
    reason = "wire fields are documented by the module security contract"
)]

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::compute::{ComputeJobLifecycle, ComputeJobPhase, ComputeJobSpec, admit_compute_job};
use crate::mesh::{
    ComputeAccelerator, DeviceKey, DeviceRole, PowerAvailability, ServerAdvertisement,
    ServiceCapabilities,
};
use crate::store::{StoreError, WorkspaceStore};

pub const COMPUTE_TRANSPORT_SCHEMA_VERSION: u16 = 1;
pub const COMPUTE_ALPN: &[u8] = b"tilo-personal-compute/1";
pub const MAX_COMPUTE_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_COMPUTE_ADVERTISEMENT_LIFETIME_MS: u64 = 60_000;
pub const MAX_COMPUTE_ADVERTISED_APPS: usize = 128;
pub const MAX_COMPUTE_ADVERTISED_APP_BYTES: usize = 32 * 1024;
pub const MAX_COMPUTE_LOG_BYTES: usize = 16 * 1024;
pub const MAX_COMPUTE_JOB_LOG_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComputeEnvelope {
    Attach {
        workspace_id: u64,
    },
    Ready {
        workspace_id: u64,
        pending: u32,
    },
    NotAuthorised {
        workspace_id: u64,
        reason: String,
    },
    Advertise {
        workspace_id: u64,
        advertisement: ComputeWorkerAdvertisement,
    },
    Request {
        spec: ComputeJobSpec,
    },
    Cancel {
        binding: ComputeJobBinding,
    },
    Status {
        update: ComputeStatusUpdate,
    },
    Result {
        result: ComputeResultUpdate,
    },
    Ping,
    Pong,
}

impl ComputeEnvelope {
    #[must_use]
    pub fn workspace_id(&self) -> Option<u64> {
        match self {
            Self::Attach { workspace_id }
            | Self::Ready { workspace_id, .. }
            | Self::NotAuthorised { workspace_id, .. }
            | Self::Advertise { workspace_id, .. } => Some(*workspace_id),
            Self::Request { spec } => Some(spec.workspace_id),
            Self::Cancel { binding } => Some(binding.workspace_id),
            Self::Status { update } => Some(update.binding.workspace_id),
            Self::Result { result } => Some(result.binding.workspace_id),
            Self::Ping | Self::Pong => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeWorkerAdvertisement {
    pub version: u16,
    pub worker: DeviceKey,
    pub workspace_id: u64,
    pub apps: BTreeSet<String>,
    pub gpu: bool,
    pub assignment_capacity: u32,
    pub active_assignments: u32,
    pub power: PowerAvailability,
    pub energy_saver: bool,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

impl ComputeWorkerAdvertisement {
    fn validate(
        &self,
        authenticated_worker: DeviceKey,
        now_ms: u64,
    ) -> Result<(), ComputeTransportError> {
        let app_bytes = self.apps.iter().map(String::len).sum::<usize>();
        if self.version != COMPUTE_TRANSPORT_SCHEMA_VERSION
            || self.worker != authenticated_worker
            || self.workspace_id == 0
            || self.apps.is_empty()
            || self.apps.len() > MAX_COMPUTE_ADVERTISED_APPS
            || app_bytes > MAX_COMPUTE_ADVERTISED_APP_BYTES
            || self.apps.iter().any(|app| !valid_app_id(app))
            || self.assignment_capacity == 0
            || self.active_assignments > self.assignment_capacity
            || self.issued_at_ms > now_ms
            || now_ms >= self.expires_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms)
                > MAX_COMPUTE_ADVERTISEMENT_LIFETIME_MS
        {
            return Err(ComputeTransportError::InvalidAdvertisement);
        }
        Ok(())
    }

    fn server_advertisement(&self) -> ServerAdvertisement {
        ServerAdvertisement {
            device: self.worker,
            role: DeviceRole::AlwaysOnServer,
            services: ServiceCapabilities {
                compute: true,
                ..ServiceCapabilities::default()
            },
            workspaces: BTreeSet::from([self.workspace_id]),
            apps: self.apps.clone(),
            accelerators: if self.gpu {
                BTreeSet::from([ComputeAccelerator::Gpu])
            } else {
                BTreeSet::new()
            },
            priority: 0,
            latency_ms: 0,
            power: self.power,
            energy_saver: self.energy_saver,
            active_assignments: self.active_assignments,
            assignment_capacity: self.assignment_capacity,
            online: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeJobBinding {
    pub workspace_id: u64,
    pub job_id: [u8; 16],
    pub requester: DeviceKey,
    pub worker: DeviceKey,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeStatusUpdate {
    pub binding: ComputeJobBinding,
    pub sequence: u64,
    pub phase: ComputeJobPhase,
    #[serde(with = "serde_bytes")]
    pub log_tail: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputeResultUpdate {
    pub binding: ComputeJobBinding,
    pub sequence: u64,
    pub phase: ComputeJobPhase,
    #[serde(with = "serde_bytes")]
    pub log_tail: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableComputeJob {
    pub spec: ComputeJobSpec,
    pub lifecycle: ComputeJobLifecycle,
    pub cancel_requested: bool,
    pub log_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComputeRouteOutcome {
    Route(DeviceKey),
    Duplicate,
}

#[derive(Debug, Error)]
pub enum ComputeTransportError {
    #[error("compute route does not match the authenticated peer")]
    RouteMismatch,
    #[error("compute worker advertisement is invalid or stale")]
    InvalidAdvertisement,
    #[error("compute worker is unavailable or ineligible")]
    WorkerUnavailable,
    #[error("compute update metadata exceeds its bound")]
    MetadataTooLarge,
    #[error("compute update is not valid for the current lifecycle")]
    InvalidLifecycle,
    #[error("compute job id was replayed with different contents")]
    ReplayConflict,
    #[error("compute job is unknown")]
    UnknownJob,
    #[error("compute store: {0}")]
    Store(#[from] StoreError),
}

#[derive(Default)]
pub struct ComputeTransportState {
    workers: HashMap<DeviceKey, ComputeWorkerAdvertisement>,
    sessions: HashMap<DeviceKey, mpsc::Sender<ComputeEnvelope>>,
}

impl ComputeTransportState {
    pub fn advertise(
        &mut self,
        authenticated_worker: DeviceKey,
        workspace_id: u64,
        now_ms: u64,
        advertisement: ComputeWorkerAdvertisement,
    ) -> Result<(), ComputeTransportError> {
        if advertisement.workspace_id != workspace_id {
            return Err(ComputeTransportError::RouteMismatch);
        }
        advertisement.validate(authenticated_worker, now_ms)?;
        self.workers.insert(authenticated_worker, advertisement);
        Ok(())
    }

    pub fn remove_worker(&mut self, worker: &DeviceKey) {
        self.workers.remove(worker);
    }

    pub fn attach(&mut self, device: DeviceKey, sender: mpsc::Sender<ComputeEnvelope>) {
        self.sessions.insert(device, sender);
    }

    pub fn detach(&mut self, device: &DeviceKey) {
        self.sessions.remove(device);
        self.workers.remove(device);
    }

    pub fn route(
        &mut self,
        target: DeviceKey,
        envelope: &ComputeEnvelope,
        store: &WorkspaceStore,
        sequence: u64,
    ) -> Result<(), ComputeTransportError> {
        if let Some(sender) = self.sessions.get(&target) {
            if sender.try_send(envelope.clone()).is_ok() {
                return Ok(());
            }
            self.detach(&target);
        }
        let bytes = postcard::to_allocvec(envelope)
            .map_err(|error| StoreError::Json(format!("encode compute envelope: {error}")))?;
        if bytes.len() > MAX_COMPUTE_FRAME_BYTES {
            return Err(ComputeTransportError::MetadataTooLarge);
        }
        store.enqueue_compute_pending(&target, sequence, &bytes)?;
        Ok(())
    }

    pub fn accept_request(
        &self,
        authenticated_requester: DeviceKey,
        workspace_id: u64,
        now_ms: u64,
        store: &WorkspaceStore,
        spec: &ComputeJobSpec,
    ) -> Result<ComputeRouteOutcome, ComputeTransportError> {
        if spec.requester != authenticated_requester || spec.workspace_id != workspace_id {
            return Err(ComputeTransportError::RouteMismatch);
        }
        let worker = self
            .workers
            .get(&spec.worker)
            .ok_or(ComputeTransportError::WorkerUnavailable)?;
        worker.validate(spec.worker, now_ms)?;
        admit_compute_job(spec, &worker.server_advertisement(), now_ms)
            .map_err(|_| ComputeTransportError::WorkerUnavailable)?;
        let record = DurableComputeJob {
            spec: spec.clone(),
            lifecycle: ComputeJobLifecycle::new(spec.job_id)
                .map_err(|_| ComputeTransportError::InvalidLifecycle)?,
            cancel_requested: false,
            log_bytes: 0,
        };
        match store.claim_compute_job(&record)? {
            None => Ok(ComputeRouteOutcome::Route(spec.worker)),
            Some(existing) if existing.spec == *spec => Ok(ComputeRouteOutcome::Duplicate),
            Some(_) => Err(ComputeTransportError::ReplayConflict),
        }
    }

    pub fn accept_cancel(
        &self,
        authenticated_requester: DeviceKey,
        store: &WorkspaceStore,
        binding: &ComputeJobBinding,
    ) -> Result<ComputeRouteOutcome, ComputeTransportError> {
        if binding.requester != authenticated_requester
            || binding.workspace_id != store.workspace_id()
        {
            return Err(ComputeTransportError::RouteMismatch);
        }
        let mut record = store
            .load_compute_job(&binding.job_id)?
            .ok_or(ComputeTransportError::UnknownJob)?;
        validate_binding(binding, &record.spec)?;
        if record.cancel_requested || record.lifecycle.phase.is_terminal() {
            return Ok(ComputeRouteOutcome::Duplicate);
        }
        record.cancel_requested = true;
        store.save_compute_job(&record)?;
        Ok(ComputeRouteOutcome::Route(binding.worker))
    }

    pub fn accept_status(
        &self,
        authenticated_worker: DeviceKey,
        store: &WorkspaceStore,
        update: &ComputeStatusUpdate,
    ) -> Result<ComputeRouteOutcome, ComputeTransportError> {
        if !matches!(
            update.phase,
            ComputeJobPhase::Pending | ComputeJobPhase::Running
        ) {
            return Err(ComputeTransportError::InvalidLifecycle);
        }
        self.accept_worker_update(
            authenticated_worker,
            store,
            &update.binding,
            update.sequence,
            update.phase.clone(),
            &update.log_tail,
        )
    }

    pub fn accept_result(
        &self,
        authenticated_worker: DeviceKey,
        store: &WorkspaceStore,
        update: &ComputeResultUpdate,
    ) -> Result<ComputeRouteOutcome, ComputeTransportError> {
        if !update.phase.is_terminal() {
            return Err(ComputeTransportError::InvalidLifecycle);
        }
        self.accept_worker_update(
            authenticated_worker,
            store,
            &update.binding,
            update.sequence,
            update.phase.clone(),
            &update.log_tail,
        )
    }

    fn accept_worker_update(
        &self,
        authenticated_worker: DeviceKey,
        store: &WorkspaceStore,
        binding: &ComputeJobBinding,
        sequence: u64,
        phase: ComputeJobPhase,
        log_tail: &[u8],
    ) -> Result<ComputeRouteOutcome, ComputeTransportError> {
        if binding.worker != authenticated_worker || binding.workspace_id != store.workspace_id() {
            return Err(ComputeTransportError::RouteMismatch);
        }
        if log_tail.len() > MAX_COMPUTE_LOG_BYTES {
            return Err(ComputeTransportError::MetadataTooLarge);
        }
        let mut record = store
            .load_compute_job(&binding.job_id)?
            .ok_or(ComputeTransportError::UnknownJob)?;
        validate_binding(binding, &record.spec)?;
        if sequence == record.lifecycle.sequence && phase == record.lifecycle.phase {
            return Ok(ComputeRouteOutcome::Duplicate);
        }
        if record.cancel_requested && !phase.is_terminal() {
            return Err(ComputeTransportError::InvalidLifecycle);
        }
        let new_log_bytes = record
            .log_bytes
            .saturating_add(u64::try_from(log_tail.len()).unwrap_or(u64::MAX));
        if new_log_bytes > MAX_COMPUTE_JOB_LOG_BYTES {
            return Err(ComputeTransportError::MetadataTooLarge);
        }
        record
            .lifecycle
            .advance(sequence, phase)
            .map_err(|_| ComputeTransportError::InvalidLifecycle)?;
        record.log_bytes = new_log_bytes;
        store.save_compute_job(&record)?;
        Ok(ComputeRouteOutcome::Route(binding.requester))
    }
}

impl ComputeJobPhase {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled
        )
    }
}

fn validate_binding(
    binding: &ComputeJobBinding,
    spec: &ComputeJobSpec,
) -> Result<(), ComputeTransportError> {
    if binding.workspace_id != spec.workspace_id
        || binding.job_id != spec.job_id
        || binding.requester != spec.requester
        || binding.worker != spec.worker
    {
        return Err(ComputeTransportError::RouteMismatch);
    }
    Ok(())
}

fn valid_app_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':' | b'/')
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::compute::{COMPUTE_JOB_SCHEMA_VERSION, ComputeResourceLimits};

    fn spec() -> ComputeJobSpec {
        ComputeJobSpec {
            version: COMPUTE_JOB_SCHEMA_VERSION,
            job_id: [9; 16],
            requester: [1; 32],
            worker: [2; 32],
            workspace_id: 7,
            app_id: "org.tilo.render".into(),
            package_digest: [3; 32],
            entrypoint: "render".into(),
            arguments: Vec::new(),
            inputs: vec![[4; 32]],
            limits: ComputeResourceLimits {
                cpu_time_ms: 5_000,
                memory_bytes: 32 * 1024 * 1024,
                output_bytes: 1024,
                gpu: false,
            },
            issued_at_ms: 100,
            expires_at_ms: 10_000,
        }
    }

    fn advertisement(gpu: bool) -> ComputeWorkerAdvertisement {
        ComputeWorkerAdvertisement {
            version: COMPUTE_TRANSPORT_SCHEMA_VERSION,
            worker: [2; 32],
            workspace_id: 7,
            apps: BTreeSet::from(["org.tilo.render".into()]),
            gpu,
            assignment_capacity: 2,
            active_assignments: 0,
            power: PowerAvailability::External,
            energy_saver: false,
            issued_at_ms: 100,
            expires_at_ms: 10_000,
        }
    }

    fn binding() -> ComputeJobBinding {
        ComputeJobBinding {
            workspace_id: 7,
            job_id: [9; 16],
            requester: [1; 32],
            worker: [2; 32],
        }
    }

    fn open_store(root: &std::path::Path) -> WorkspaceStore {
        WorkspaceStore::open(root, 7, false).unwrap()
    }

    fn admitted(root: &std::path::Path) -> (ComputeTransportState, WorkspaceStore) {
        let mut state = ComputeTransportState::default();
        state
            .advertise([2; 32], 7, 200, advertisement(false))
            .unwrap();
        let store = open_store(root);
        assert_eq!(
            state
                .accept_request([1; 32], 7, 200, &store, &spec())
                .unwrap(),
            ComputeRouteOutcome::Route([2; 32])
        );
        (state, store)
    }

    #[test]
    fn request_binding_survives_restart_and_replay_is_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let (state, store) = admitted(root.path());
        assert_eq!(
            state
                .accept_request([1; 32], 7, 200, &store, &spec())
                .unwrap(),
            ComputeRouteOutcome::Duplicate
        );

        let mut restarted = ComputeTransportState::default();
        restarted
            .advertise([2; 32], 7, 200, advertisement(false))
            .unwrap();
        assert_eq!(
            restarted
                .accept_request([1; 32], 7, 200, &open_store(root.path()), &spec())
                .unwrap(),
            ComputeRouteOutcome::Duplicate
        );

        let mut conflict = spec();
        conflict.package_digest = [8; 32];
        assert!(matches!(
            restarted.accept_request([1; 32], 7, 200, &store, &conflict),
            Err(ComputeTransportError::ReplayConflict)
        ));
    }

    #[test]
    fn forged_route_and_stale_or_ineligible_worker_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let store = open_store(root.path());
        let mut state = ComputeTransportState::default();
        state
            .advertise([2; 32], 7, 200, advertisement(false))
            .unwrap();
        assert!(matches!(
            state.accept_request([5; 32], 7, 200, &store, &spec()),
            Err(ComputeTransportError::RouteMismatch)
        ));
        assert!(matches!(
            state.accept_request([1; 32], 7, 10_000, &store, &spec()),
            Err(ComputeTransportError::InvalidAdvertisement)
        ));

        let mut gpu_job = spec();
        gpu_job.job_id = [8; 16];
        gpu_job.limits.gpu = true;
        assert!(matches!(
            state.accept_request([1; 32], 7, 200, &store, &gpu_job),
            Err(ComputeTransportError::WorkerUnavailable)
        ));
        state.remove_worker(&[2; 32]);
        assert!(matches!(
            state.accept_request([1; 32], 7, 200, &store, &spec()),
            Err(ComputeTransportError::WorkerUnavailable)
        ));
    }

    #[test]
    fn cancellation_is_durable_idempotent_and_route_bound() {
        let root = tempfile::tempdir().unwrap();
        let (state, store) = admitted(root.path());
        assert_eq!(
            state.accept_cancel([1; 32], &store, &binding()).unwrap(),
            ComputeRouteOutcome::Route([2; 32])
        );
        assert_eq!(
            state.accept_cancel([1; 32], &store, &binding()).unwrap(),
            ComputeRouteOutcome::Duplicate
        );
        assert!(
            open_store(root.path())
                .load_compute_job(&[9; 16])
                .unwrap()
                .unwrap()
                .cancel_requested
        );
        assert!(matches!(
            state.accept_cancel([3; 32], &store, &binding()),
            Err(ComputeTransportError::RouteMismatch)
        ));
    }

    #[test]
    fn status_result_are_monotonic_bounded_and_worker_bound() {
        let root = tempfile::tempdir().unwrap();
        let (state, store) = admitted(root.path());
        let status = ComputeStatusUpdate {
            binding: binding(),
            sequence: 1,
            phase: ComputeJobPhase::Running,
            log_tail: b"started".to_vec(),
        };
        assert_eq!(
            state.accept_status([2; 32], &store, &status).unwrap(),
            ComputeRouteOutcome::Route([1; 32])
        );
        assert_eq!(
            state.accept_status([2; 32], &store, &status).unwrap(),
            ComputeRouteOutcome::Duplicate
        );
        assert!(matches!(
            state.accept_status([6; 32], &store, &status),
            Err(ComputeTransportError::RouteMismatch)
        ));

        let mut oversized = status.clone();
        oversized.sequence = 2;
        oversized.log_tail = vec![0; MAX_COMPUTE_LOG_BYTES + 1];
        assert!(matches!(
            state.accept_status([2; 32], &store, &oversized),
            Err(ComputeTransportError::MetadataTooLarge)
        ));

        let result = ComputeResultUpdate {
            binding: binding(),
            sequence: 2,
            phase: ComputeJobPhase::Completed {
                outputs: vec![[7; 32]],
            },
            log_tail: b"done".to_vec(),
        };
        assert_eq!(
            state.accept_result([2; 32], &store, &result).unwrap(),
            ComputeRouteOutcome::Route([1; 32])
        );
        assert_eq!(
            state.accept_result([2; 32], &store, &result).unwrap(),
            ComputeRouteOutcome::Duplicate
        );
    }

    #[test]
    fn partitioned_route_queues_on_dedicated_protocol_and_live_route_does_not() {
        let root = tempfile::tempdir().unwrap();
        let store = open_store(root.path());
        let mut state = ComputeTransportState::default();
        let envelope = ComputeEnvelope::Cancel { binding: binding() };
        state.route([2; 32], &envelope, &store, 1).unwrap();
        let pending = store.load_compute_pending(&[2; 32]).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            postcard::from_bytes::<ComputeEnvelope>(&pending[0].bytes).unwrap(),
            envelope
        );

        let (sender, mut receiver) = mpsc::channel(1);
        state.attach([2; 32], sender);
        let live = ComputeEnvelope::Ping;
        state.route([2; 32], &live, &store, 2).unwrap();
        assert_eq!(receiver.try_recv().unwrap(), live);
        assert_eq!(store.load_compute_pending(&[2; 32]).unwrap().len(), 1);
    }
}
