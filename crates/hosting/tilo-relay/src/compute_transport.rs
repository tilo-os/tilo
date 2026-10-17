//! Authenticated, durable routing for personal-mesh compute metadata.
//!
//! Package and content bytes never travel in these messages: requests name
//! signed packages and content digests, while results contain only output
//! digests and a tightly bounded diagnostic tail. The relay derives the
//! sender from the certified connection and persists the exact route binding
//! before forwarding anything. The bytes behind those digests move on
//! job-scoped streams of their own ([`crate::compute_content`]), authorised
//! by the binding persisted here.

#![allow(
    missing_docs,
    reason = "wire fields are documented by the module security contract"
)]

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::compute::{ComputeJobLifecycle, ComputeJobPhase, ComputeJobSpec, admit_compute_job};
use crate::mesh::{
    ComputeAccelerator, DeviceKey, DeviceRole, PowerAvailability, ServerAdvertisement,
    ServiceCapabilities,
};
use crate::placement::{
    ComputeRequirements, NoEligibleWorker, Placement, PlacementRank, place_compute_job,
};
use crate::store::{StoreError, WorkspaceStore};

pub const COMPUTE_TRANSPORT_SCHEMA_VERSION: u16 = 1;
pub use tilo_relay_protocol::transport::COMPUTE_ALPN;
pub const MAX_COMPUTE_FRAME_BYTES: usize = 256 * 1024;
pub const MAX_COMPUTE_ADVERTISEMENT_LIFETIME_MS: u64 = 60_000;
pub const MAX_COMPUTE_ADVERTISED_APPS: usize = 128;
pub const MAX_COMPUTE_ADVERTISED_APP_BYTES: usize = 32 * 1024;
pub const MAX_COMPUTE_LOG_BYTES: usize = 16 * 1024;
pub const MAX_COMPUTE_JOB_LOG_BYTES: u64 = 64 * 1024;
/// The latency placement ranks a worker by while the relay has no RTT
/// observation of it: after every measured worker, never ahead of one.
pub const UNMEASURED_LATENCY_MS: u32 = u32::MAX;
/// RTT observations above this are clamped: a path that slow is reported as
/// this slow, and one stray sample cannot overflow the smoothed estimate.
const MAX_OBSERVED_RTT: Duration = Duration::from_secs(60);
/// Each new observation moves the smoothed RTT by this fraction of the
/// difference, rounded up so sustained samples converge exactly.
const RTT_SMOOTHING_DIVISOR: u64 = 4;
/// A content transfer smaller than this says more about round trips than
/// about the link's rate, so it is not a throughput observation.
pub const MIN_THROUGHPUT_SAMPLE_BYTES: u64 = 256 * 1024;
/// Why a job ended because its worker went away: the connection ended.
pub const WORKER_LOST: &str = "worker_lost";
/// ... because the worker's device certificate was revoked.
pub const WORKER_REVOKED: &str = "worker_revoked";
/// ... because the worker is no longer admitted to the workspace for any
/// other reason (its certificate expired, it left the membership).
pub const WORKER_NOT_ADMITTED: &str = "worker_not_admitted";
/// Why a job ended because the relay restarted while it was unfinished.
/// A worker loses its jobs with its session to the relay, so nothing can
/// still be running one once the relay is back.
pub const RELAY_RESTARTED: &str = "relay_restarted";
/// The placement reason when moving a job's inputs to every eligible
/// worker would take longer than running the job where it was asked.
pub const INPUTS_HEAVIER_THAN_GAIN: &str = "inputs_heavier_than_gain";
/// The placement reason when a worker kept from jobs by what it was
/// expected to receive is tried again, in case it holds the inputs now.
pub const REPROBING_HELD_INPUTS: &str = "reprobing_held_inputs";
/// How long placement keeps jobs from a worker on what it last learned the
/// worker holds before it may try that worker again (see `Reprobe`).
pub const REPROBE_AFTER_MS: u64 = 10 * 60_000;
/// A re-probe that finds nothing held doubles both the wait and the
/// forgone gain the next one needs, at most this many times over.
pub const MAX_REPROBE_BACKOFF: u32 = 6;
/// Most input digests the relay remembers a worker holding, or not, for one
/// requester; the one learned first is forgotten first.
pub const MAX_KNOWN_HOLDINGS: usize = 4_096;
/// Most live jobs whose input movements the relay follows at once. Past
/// it a new job is not followed, and teaches placement nothing.
const MAX_FOLLOWED_JOBS: usize = 1_024;

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
    Delivery {
        workspace_id: u64,
        sequence: u64,
        message: Box<ComputeEnvelope>,
    },
    Ack {
        workspace_id: u64,
        sequence: u64,
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
            | Self::Advertise { workspace_id, .. }
            | Self::Delivery { workspace_id, .. }
            | Self::Ack { workspace_id, .. } => Some(*workspace_id),
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
    pub accelerators: BTreeSet<ComputeAccelerator>,
    pub assignment_capacity: u32,
    pub active_assignments: u32,
    /// Interactive environment sessions the worker hosts at once; zero when
    /// it hosts none.
    pub environment_capacity: u32,
    pub active_environments: u32,
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
            || (self.apps.is_empty()
                && self.environment_capacity == 0
                && !self.accelerators.contains(&ComputeAccelerator::Kvm))
            || self.apps.len() > MAX_COMPUTE_ADVERTISED_APPS
            || app_bytes > MAX_COMPUTE_ADVERTISED_APP_BYTES
            || self.apps.iter().any(|app| !valid_app_id(app))
            || self.assignment_capacity == 0
            || self.active_assignments > self.assignment_capacity
            || self.active_environments > self.environment_capacity
            || self.issued_at_ms > now_ms
            || now_ms >= self.expires_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms)
                > MAX_COMPUTE_ADVERTISEMENT_LIFETIME_MS
        {
            return Err(ComputeTransportError::InvalidAdvertisement);
        }
        Ok(())
    }

    fn server_advertisement(&self, latency_ms: u32) -> ServerAdvertisement {
        ServerAdvertisement {
            device: self.worker,
            role: DeviceRole::AlwaysOnServer,
            services: ServiceCapabilities {
                compute: true,
                ..ServiceCapabilities::default()
            },
            workspaces: BTreeSet::from([self.workspace_id]),
            apps: self.apps.clone(),
            accelerators: self.accelerators.clone(),
            priority: 0,
            latency_ms,
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

/// A worker's relay-to-worker round trip, smoothed across observations.
///
/// Fed only from what the transport already measures: the QUIC RTT estimate
/// of the worker's compute connection, read when its periodic advertisement
/// arrives. Measuring adds no packet, task or timer, so an idle relay stays
/// idle. Latency is a routing hint; eligibility never depends on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SmoothedRtt {
    micros: Option<u64>,
}

impl SmoothedRtt {
    fn observe(&mut self, sample: Duration) {
        let sample = u64::try_from(sample.min(MAX_OBSERVED_RTT).as_micros()).unwrap_or(u64::MAX);
        self.micros =
            Some(match self.micros {
                None => sample,
                Some(smoothed) if sample >= smoothed => smoothed
                    .saturating_add(sample.abs_diff(smoothed).div_ceil(RTT_SMOOTHING_DIVISOR)),
                Some(smoothed) => smoothed
                    .saturating_sub(smoothed.abs_diff(sample).div_ceil(RTT_SMOOTHING_DIVISOR)),
            });
    }

    /// Whole milliseconds, rounded up so a measured worker never reads as
    /// the zero an unmeasured one used to.
    fn millis(self) -> u32 {
        self.micros.map_or(UNMEASURED_LATENCY_MS, |micros| {
            u32::try_from(micros.div_ceil(1_000).max(1))
                .unwrap_or(UNMEASURED_LATENCY_MS.saturating_sub(1))
        })
    }
}

/// A requester-to-worker content rate, smoothed across transfers.
///
/// Fed from the job content the relay already forwards
/// ([`crate::compute_content`]): each input moved from requester to worker
/// that is large enough to say something about the link is one
/// observation. Inputs a worker already holds move nothing and say
/// nothing. Like latency, it is a routing hint and grants nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SmoothedThroughput {
    bytes_per_second: Option<u64>,
}

impl SmoothedThroughput {
    fn observe(&mut self, bytes: u64, elapsed: Duration) {
        let micros = u64::try_from(elapsed.as_micros())
            .unwrap_or(u64::MAX)
            .max(1);
        let sample = u64::try_from(
            u128::from(bytes)
                .saturating_mul(1_000_000)
                .checked_div(u128::from(micros))
                .unwrap_or(u128::MAX),
        )
        .unwrap_or(u64::MAX)
        .max(1);
        self.bytes_per_second = Some(match self.bytes_per_second {
            None => sample,
            Some(smoothed) if sample >= smoothed => {
                smoothed.saturating_add(sample.abs_diff(smoothed).div_ceil(RTT_SMOOTHING_DIVISOR))
            }
            Some(smoothed) => smoothed
                .saturating_sub(smoothed.abs_diff(sample).div_ceil(RTT_SMOOTHING_DIVISOR))
                .max(1),
        });
    }
}

/// What a job asks to move against what it costs where it was asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputWeight {
    /// The bytes of every input the job lists.
    pub input_bytes: u64,
    /// The requester's own wall time for this action over inputs like
    /// these: what offloading has to beat.
    pub local_wall_ms: u64,
    /// The job's distinct inputs by digest, each with its bytes, when the
    /// requester named them; their bytes are part of `input_bytes`. An
    /// input a worker is known to hold is not weighed as moving to it.
    pub inputs: Vec<([u8; 32], u64)>,
}

/// Which of one requester's inputs a worker was seen to hold: a routing
/// hint the relay learns from the jobs it carried, never from the
/// worker's word.
///
/// An input counts as held when, in a job that completed, the requester
/// proved to the worker that it holds the bytes (the proof route of
/// [`crate::compute_content`]) and the relay then forwarded none of them:
/// the worker staged its own copy, which it does only when that copy
/// reproduces the proof. An input the relay forwarded is not held. Knowing
/// this grants nothing: every job still proves possession or fetches, so a
/// hint gone wrong (the worker evicted its copy) costs one slower run, and
/// that run unlearns it.
#[derive(Clone, Debug, Default)]
struct Holdings {
    /// Each input learned, and whether the worker held it.
    known: HashMap<[u8; 32], bool>,
    /// The order inputs were first learned in, oldest first.
    order: VecDeque<[u8; 32]>,
    /// The share of its last completed job's inputs the worker held, per
    /// mille: what is expected of an input not learned yet. A replica that
    /// keeps a workspace's originals holds the next photograph too.
    held_per_mille: u64,
    /// When to try the worker again although this says its inputs move.
    reprobe: Reprobe,
}

/// When to try a worker again that placement keeps jobs from because of
/// what it was seen to fetch.
///
/// What a worker holds is learned only from jobs placed on it, so a worker
/// seen fetching everything over a slow link would otherwise never be tried
/// again, even once it holds the inputs (its filesystem replica finished
/// syncing). Each job kept local instead is credited with what it would
/// have saved had the worker held its inputs. Once [`REPROBE_AFTER_MS`]
/// has passed and that forgone gain covers the worst case of trying (the
/// estimate is right and every byte moves), the next such job is placed on
/// the worker; the job itself shows what it holds, as any other does.
///
/// A re-probe that finds nothing held doubles the wait and the credit the
/// next one needs, up to [`MAX_REPROBE_BACKOFF`] times, so a worker that
/// still holds nothing goes back to staying local, and the time re-probes
/// waste never exceeds the time staying local already forwent. A job that
/// finds anything held resets it. Driven only by placement calls and
/// settled jobs: no timer, and a fixed size per (requester, worker) pair.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Reprobe {
    /// When placement first kept a job from the worker since the pair last
    /// learned from a job or was re-probed.
    since_ms: Option<u64>,
    /// What the jobs kept local since then would have saved, in ms, had the
    /// worker held their inputs.
    forgone_ms: u64,
    /// Re-probes since a job last found inputs held, at most
    /// [`MAX_REPROBE_BACKOFF`].
    fruitless: u32,
}

impl Reprobe {
    fn backoff(self) -> u64 {
        1_u64
            .checked_shl(self.fruitless.min(MAX_REPROBE_BACKOFF))
            .unwrap_or(u64::MAX)
    }

    /// A job was kept from the worker at `now_ms`, which would have saved
    /// `gain_ms` had the worker held its inputs and costs `cost_ms` more
    /// than the local run if it does not. Returns whether to try it now.
    fn kept_local(&mut self, now_ms: u64, gain_ms: u64, cost_ms: u64) -> bool {
        let since = *self.since_ms.get_or_insert(now_ms);
        self.forgone_ms = self.forgone_ms.saturating_add(gain_ms);
        let backoff = self.backoff();
        now_ms.saturating_sub(since) >= REPROBE_AFTER_MS.saturating_mul(backoff)
            && self.forgone_ms >= cost_ms.saturating_mul(backoff)
    }

    /// The worker is tried again at `now_ms`; it counts as fruitless until
    /// a job finds inputs held.
    fn probed(&mut self, now_ms: u64) {
        self.since_ms = Some(now_ms);
        self.forgone_ms = 0;
        self.fruitless = self.fruitless.saturating_add(1).min(MAX_REPROBE_BACKOFF);
    }

    /// A job on the worker completed; it found inputs held or not.
    fn learned(&mut self, held_any: bool) {
        self.since_ms = None;
        self.forgone_ms = 0;
        if held_any {
            self.fruitless = 0;
        }
    }
}

impl Holdings {
    fn learn(&mut self, digest: [u8; 32], held: bool) {
        if self.known.insert(digest, held).is_none() {
            self.order.push_back(digest);
        }
        while self.order.len() > MAX_KNOWN_HOLDINGS {
            if let Some(oldest) = self.order.pop_front() {
                self.known.remove(&oldest);
            }
        }
    }

    /// Of `bytes` of inputs not learned yet, those expected to move.
    fn unknown_moving(&self, bytes: u64) -> u64 {
        let moving = u128::from(bytes)
            .saturating_mul(u128::from(1_000_u64.saturating_sub(self.held_per_mille)))
            .div_ceil(1_000);
        u64::try_from(moving).unwrap_or(u64::MAX)
    }

    /// The bytes of `weight`'s inputs this worker is expected to receive.
    fn moving(&self, weight: &InputWeight) -> u64 {
        let listed = weight
            .inputs
            .iter()
            .fold(0_u64, |sum, (_, bytes)| sum.saturating_add(*bytes));
        let mut moving = self.unknown_moving(weight.input_bytes.saturating_sub(listed));
        for (digest, bytes) in &weight.inputs {
            moving = moving.saturating_add(match self.known.get(digest) {
                Some(true) => 0,
                Some(false) => *bytes,
                None => self.unknown_moving(*bytes),
            });
        }
        moving
    }
}

/// The job an input moved for, as its durable record binds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobScope {
    pub job_id: [u8; 16],
    /// The job's requester and worker.
    pub pair: (DeviceKey, DeviceKey),
    pub expires_at_ms: u64,
}

/// What the relay carried for one live job's inputs.
#[derive(Clone, Debug)]
struct FollowedJob {
    scope: JobScope,
    /// Inputs the requester proved to the worker it holds.
    proven: BTreeSet<[u8; 32]>,
    /// Inputs the relay forwarded from the requester to the worker.
    forwarded: BTreeSet<[u8; 32]>,
}

/// An eligible worker whose inputs would move slower than the local run.
struct Refused {
    worker: DeviceKey,
    rank: PlacementRank,
    /// The bytes that would move to it.
    moving: u64,
    transfer_ms: u64,
    bytes_per_second: u64,
}

impl Refused {
    fn estimate(
        &self,
        weight: &InputWeight,
        considered: Vec<crate::placement::PlacementCandidate>,
    ) -> InputsHeavierThanGain {
        InputsHeavierThanGain {
            worker: self.worker,
            input_bytes: self.moving,
            held_bytes: weight.input_bytes.saturating_sub(self.moving),
            bytes_per_second: self.bytes_per_second,
            transfer_ms: self.transfer_ms,
            local_wall_ms: weight.local_wall_ms,
            considered,
        }
    }
}

/// Placement weighed against moving the inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WeighedPlacement {
    /// Run it on this worker.
    Worker(Placement),
    /// Run it where it was asked: moving the inputs to every eligible
    /// worker takes longer than the run there took.
    Local(InputsHeavierThanGain),
    /// Run it on this worker although the estimate says its inputs move
    /// slower than the run where it was asked: the worker was kept from
    /// such jobs long enough to try it again (see `Reprobe`).
    Reprobe(ReprobePlacement),
}

/// A worker tried again in case it holds a job's inputs now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReprobePlacement {
    /// The worker tried, as placement ranked it.
    pub placement: Placement,
    /// The estimate that would have kept the job local.
    pub estimate: InputsHeavierThanGain,
}

impl std::fmt::Display for ReprobePlacement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{REPROBING_HELD_INPUTS}: {}; jobs kept here since it was last tried would have \
             gained enough to try it again, in case it holds the inputs now (estimate: {})",
            self.placement, self.estimate,
        )
    }
}

/// Why a job is better run by its requester: the estimate for the
/// best-ranked eligible worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputsHeavierThanGain {
    /// The best-ranked eligible worker.
    pub worker: DeviceKey,
    /// What the job would move to it.
    pub input_bytes: u64,
    /// The bytes of the job's inputs that worker is expected to hold
    /// already, which would not move.
    pub held_bytes: u64,
    /// The measured requester-to-worker rate.
    pub bytes_per_second: u64,
    /// The estimated time to move the inputs, round trip included.
    pub transfer_ms: u64,
    /// The requester's own wall time it would have to beat.
    pub local_wall_ms: u64,
    /// Every advertised device, as placement ranked it.
    pub considered: Vec<crate::placement::PlacementCandidate>,
}

impl std::fmt::Display for InputsHeavierThanGain {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{INPUTS_HEAVIER_THAN_GAIN}: moving {} bytes to {} ({} more held there) takes about \
             {} ms at {} bytes/s; the requester ran it in {} ms",
            self.input_bytes,
            hex::encode(self.worker.get(..4).unwrap_or_default()),
            self.held_bytes,
            self.transfer_ms,
            self.bytes_per_second,
            self.local_wall_ms,
        )
    }
}

/// The RTT of the path a connection sends on now, or the best open path's
/// when none is selected; `None` once the connection has closed.
fn connection_rtt(connection: &iroh::endpoint::Connection) -> Option<Duration> {
    if connection.close_reason().is_some() {
        return None;
    }
    let paths = connection.paths();
    let mut fastest = None::<Duration>;
    for path in &paths {
        let rtt = path.rtt();
        if path.is_selected() {
            return Some(rtt);
        }
        fastest = Some(fastest.map_or(rtt, |best| best.min(rtt)));
    }
    fastest
}

#[derive(Default)]
pub struct ComputeTransportState {
    workers: HashMap<DeviceKey, ComputeWorkerAdvertisement>,
    sessions: HashMap<DeviceKey, ComputeSubscriber>,
    /// Smoothed relay-to-worker RTT per attached worker connection.
    latency: HashMap<DeviceKey, SmoothedRtt>,
    /// Smoothed content rate per (requester, worker) pair.
    throughput: HashMap<(DeviceKey, DeviceKey), SmoothedThroughput>,
    /// Which of a requester's inputs a worker was seen to hold, per
    /// (requester, worker) pair.
    holdings: HashMap<(DeviceKey, DeviceKey), Holdings>,
    /// The input movements of live jobs, until each settles.
    followed: HashMap<[u8; 16], FollowedJob>,
    /// Members' environment sessions and legacy launches routed to workers.
    pub environments: crate::environment_routing::WorkspaceEnvironments,
}

struct ComputeSubscriber {
    sender: mpsc::Sender<ComputeEnvelope>,
    disconnect: mpsc::Sender<()>,
    connection: Option<iroh::endpoint::Connection>,
}

impl ComputeTransportState {
    /// Publish or refresh an authenticated worker advertisement.
    ///
    /// # Errors
    ///
    /// Returns an error when the advertisement is malformed, expired, or does
    /// not belong to the authenticated worker and workspace.
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
        // The advertisement is the worker's heartbeat; sampling the RTT the
        // transport already keeps here costs no wakeup of its own.
        if let Some(rtt) = self
            .sessions
            .get(&authenticated_worker)
            .and_then(|subscriber| subscriber.connection.as_ref())
            .and_then(connection_rtt)
        {
            self.observe_latency(authenticated_worker, rtt);
        }
        Ok(())
    }

    /// Fold one relay-to-worker RTT observation into the worker's smoothed
    /// latency.
    fn observe_latency(&mut self, worker: DeviceKey, rtt: Duration) {
        self.latency.entry(worker).or_default().observe(rtt);
    }

    /// The smoothed relay-to-worker latency placement ranks `worker` by, or
    /// [`UNMEASURED_LATENCY_MS`] before the relay has observed it.
    #[must_use]
    pub fn latency_ms(&self, worker: &DeviceKey) -> u32 {
        self.latency
            .get(worker)
            .copied()
            .unwrap_or_default()
            .millis()
    }

    /// Choose the worker for a job needing `needs` among the workers
    /// advertising in this workspace, by the same eligibility rule
    /// [`Self::accept_request`] admits by. An advertisement that no longer
    /// validates (expired, or never fresh) counts as offline.
    ///
    /// # Errors
    ///
    /// Returns [`NoEligibleWorker`], with each advertised worker's reason,
    /// when none can take the job.
    pub fn place(
        &self,
        needs: &ComputeRequirements<'_>,
        now_ms: u64,
    ) -> Result<Placement, NoEligibleWorker> {
        let advertised = self
            .workers
            .iter()
            .map(|(worker, advertisement)| {
                let mut server = advertisement.server_advertisement(self.latency_ms(worker));
                server.online = advertisement.validate(*worker, now_ms).is_ok();
                server
            })
            .collect::<Vec<_>>();
        place_compute_job(needs, &advertised)
    }

    /// Fold one forwarded content transfer between a job's `requester` and
    /// its `worker` into their smoothed rate. Transfers below
    /// [`MIN_THROUGHPUT_SAMPLE_BYTES`] are ignored.
    pub fn observe_transfer(
        &mut self,
        (requester, worker): (DeviceKey, DeviceKey),
        bytes: u64,
        elapsed: Duration,
    ) {
        if bytes < MIN_THROUGHPUT_SAMPLE_BYTES {
            return;
        }
        self.throughput
            .entry((requester, worker))
            .or_default()
            .observe(bytes, elapsed);
    }

    /// The smoothed content rate between `requester` and `worker` in bytes
    /// per second, once the relay has forwarded enough between them.
    #[must_use]
    pub fn throughput(&self, requester: &DeviceKey, worker: &DeviceKey) -> Option<u64> {
        self.throughput
            .get(&(*requester, *worker))
            .and_then(|smoothed| smoothed.bytes_per_second)
    }

    /// How long moving `input_bytes` from `requester` to `worker` should
    /// take, with the rate it assumes; `None` while the rate is unmeasured.
    fn transfer_estimate(
        &self,
        requester: &DeviceKey,
        worker: &DeviceKey,
        input_bytes: u64,
    ) -> Option<(u64, u64)> {
        let rate = self.throughput(requester, worker)?;
        let moving = u128::from(input_bytes)
            .saturating_mul(1_000)
            .div_ceil(u128::from(rate.max(1)));
        Some((
            u64::try_from(moving.saturating_add(u128::from(self.round_trip_ms(worker))))
                .unwrap_or(u64::MAX),
            rate,
        ))
    }

    /// What reaching `worker` costs a job that moves nothing: its measured
    /// round trip, or nothing while unmeasured.
    fn round_trip_ms(&self, worker: &DeviceKey) -> u64 {
        match self.latency_ms(worker) {
            UNMEASURED_LATENCY_MS => 0,
            measured => u64::from(measured),
        }
    }

    /// The bytes of `weight`'s inputs that would move from `requester` to
    /// `worker`: all of them, less those it was seen to hold
    /// ([`Holdings`]).
    fn moving_bytes(&self, requester: &DeviceKey, worker: &DeviceKey, weight: &InputWeight) -> u64 {
        self.holdings
            .get(&(*requester, *worker))
            .map_or(weight.input_bytes, |holdings| holdings.moving(weight))
    }

    /// [`Self::place`], weighed against moving the inputs: the
    /// best-ranked eligible worker whose inputs move faster than the
    /// requester ran the job itself, or [`WeighedPlacement::Local`] when
    /// none does. Only what a worker would receive is weighed: inputs it
    /// was seen to hold do not move, so a worker that holds them all costs
    /// the round trip alone, however slow its link. A worker whose rate is
    /// not measured yet is not held back: offloading to it is how it gets
    /// measured. Without a `weight` this is [`Self::place`].
    ///
    /// What a worker holds is learned only from jobs placed on it, so a
    /// worker kept from jobs this way is tried again once that has forgone
    /// enough, for long enough: [`WeighedPlacement::Reprobe`], after
    /// [`REPROBE_AFTER_MS`] at the earliest. That is why placing takes
    /// `&mut self`; `now_ms` is the clock it is judged by.
    ///
    /// # Errors
    ///
    /// As [`Self::place`].
    pub fn place_weighed(
        &mut self,
        needs: &ComputeRequirements<'_>,
        weight: Option<&InputWeight>,
        now_ms: u64,
    ) -> Result<WeighedPlacement, NoEligibleWorker> {
        let mut placement = self.place(needs, now_ms)?;
        let Some(weight) = weight else {
            return Ok(WeighedPlacement::Worker(placement));
        };
        let mut refused = Vec::new();
        for candidate in &placement.considered {
            let Ok(rank) = candidate.verdict else {
                continue;
            };
            let moving = self.moving_bytes(&needs.requester, &candidate.device, weight);
            match self.transfer_estimate(&needs.requester, &candidate.device, moving) {
                Some((transfer_ms, bytes_per_second)) if transfer_ms >= weight.local_wall_ms => {
                    refused.push(Refused {
                        worker: candidate.device,
                        rank,
                        moving,
                        transfer_ms,
                        bytes_per_second,
                    });
                }
                _ => {
                    placement.worker = candidate.device;
                    placement.rank = rank;
                    return Ok(WeighedPlacement::Worker(placement));
                }
            }
        }
        if let Some(tried) =
            self.reprobe_due(needs.requester, weight.local_wall_ms, &refused, now_ms)
        {
            let estimate = tried.estimate(weight, placement.considered.clone());
            placement.worker = tried.worker;
            placement.rank = tried.rank;
            return Ok(WeighedPlacement::Reprobe(ReprobePlacement {
                placement,
                estimate,
            }));
        }
        Ok(WeighedPlacement::Local(match refused.first() {
            Some(best) => best.estimate(weight, placement.considered),
            None => InputsHeavierThanGain {
                worker: placement.worker,
                input_bytes: weight.input_bytes,
                held_bytes: 0,
                bytes_per_second: 0,
                transfer_ms: u64::MAX,
                local_wall_ms: weight.local_wall_ms,
                considered: placement.considered,
            },
        }))
    }

    /// Credit each worker `refused` (best first) with what the job kept from
    /// it would have saved had it held the inputs, and return the first one
    /// due a re-probe ([`Reprobe`]), now marked as tried.
    ///
    /// A worker is refused only over a measured rate, so this keeps state
    /// only for pairs the relay has carried content between.
    fn reprobe_due<'refused>(
        &mut self,
        requester: DeviceKey,
        local_wall_ms: u64,
        refused: &'refused [Refused],
        now_ms: u64,
    ) -> Option<&'refused Refused> {
        for candidate in refused {
            // Holding everything leaves only the round trip; a worker that
            // would not beat the local run even then has nothing to gain.
            let gain_ms = local_wall_ms.saturating_sub(self.round_trip_ms(&candidate.worker));
            if gain_ms == 0 {
                continue;
            }
            let cost_ms = candidate.transfer_ms.saturating_sub(local_wall_ms);
            let reprobe = &mut self
                .holdings
                .entry((requester, candidate.worker))
                .or_default()
                .reprobe;
            if reprobe.kept_local(now_ms, gain_ms, cost_ms) {
                reprobe.probed(now_ms);
                return Some(candidate);
            }
        }
        None
    }

    /// The live job `scope` names, followed from now on; `None` when the
    /// relay already follows as many as it will.
    fn follow(&mut self, scope: JobScope, now_ms: u64) -> Option<&mut FollowedJob> {
        if !self.followed.contains_key(&scope.job_id) && self.followed.len() >= MAX_FOLLOWED_JOBS {
            self.followed
                .retain(|_, job| job.scope.expires_at_ms > now_ms);
        }
        if !self.followed.contains_key(&scope.job_id) && self.followed.len() >= MAX_FOLLOWED_JOBS {
            return None;
        }
        let job = self.followed.entry(scope.job_id).or_insert(FollowedJob {
            scope,
            proven: BTreeSet::new(),
            forwarded: BTreeSet::new(),
        });
        (job.scope == scope).then_some(job)
    }

    /// The requester of the job `scope` names proved to its worker that it
    /// holds input `digest`. Whether the worker held it too shows when the
    /// job completes: it did unless the relay forwarded the bytes.
    pub fn observe_proof(&mut self, scope: JobScope, digest: [u8; 32], now_ms: u64) {
        if let Some(job) = self.follow(scope, now_ms) {
            job.proven.insert(digest);
        }
    }

    /// The relay is forwarding input `digest` of the job `scope` names from
    /// its requester to its worker: the worker did not hold it. Noted
    /// before the first byte moves, so the job cannot complete first.
    pub fn observe_forwarded(&mut self, scope: JobScope, digest: [u8; 32], now_ms: u64) {
        if let Some(job) = self.follow(scope, now_ms) {
            job.forwarded.insert(digest);
        }
    }

    /// Stop following job `job_id` of `worker`. When it `completed`, learn
    /// from it which inputs the worker held ([`Holdings`]).
    fn settle(&mut self, job_id: &[u8; 16], worker: &DeviceKey, completed: bool) {
        let Some(job) = self.followed.remove(job_id) else {
            return;
        };
        if !completed || job.scope.pair.1 != *worker {
            return;
        }
        let held = job
            .proven
            .difference(&job.forwarded)
            .copied()
            .collect::<Vec<_>>();
        let total = held.len().saturating_add(job.forwarded.len());
        if total == 0 {
            return;
        }
        let holdings = self.holdings.entry(job.scope.pair).or_default();
        for digest in &held {
            holdings.learn(*digest, true);
        }
        for digest in &job.forwarded {
            holdings.learn(*digest, false);
        }
        holdings.reprobe.learned(!held.is_empty());
        let held = u64::try_from(held.len()).unwrap_or(u64::MAX);
        let total = u64::try_from(total).unwrap_or(u64::MAX);
        holdings.held_per_mille = held.saturating_mul(1_000).checked_div(total).unwrap_or(0);
    }

    pub fn remove_worker(&mut self, worker: &DeviceKey) {
        self.workers.remove(worker);
    }

    pub fn attach(
        &mut self,
        device: DeviceKey,
        sender: mpsc::Sender<ComputeEnvelope>,
        disconnect: mpsc::Sender<()>,
        connection: Option<iroh::endpoint::Connection>,
    ) {
        // A new connection is a new path; the old one's RTT and rate say
        // nothing. A worker that reattaches may have restarted on other
        // storage, so what it was seen to hold is unknown again too.
        self.latency.remove(&device);
        self.throughput
            .retain(|(requester, worker), _| *requester != device && *worker != device);
        self.holdings.retain(|(_, worker), _| *worker != device);
        self.followed.retain(|_, job| job.scope.pair.1 != device);
        self.sessions.insert(
            device,
            ComputeSubscriber {
                sender,
                disconnect,
                connection,
            },
        );
    }

    /// The live connection of an attached worker that currently advertises
    /// room for one more environment session in this workspace, least
    /// loaded first.
    #[must_use]
    pub fn environment_worker(
        &self,
        now_ms: u64,
    ) -> Option<(DeviceKey, iroh::endpoint::Connection)> {
        self.workers
            .iter()
            .filter(|(worker, advertisement)| {
                advertisement.validate(**worker, now_ms).is_ok()
                    && advertisement.active_environments < advertisement.environment_capacity
                    && !advertisement.energy_saver
            })
            .min_by_key(|(worker, advertisement)| (advertisement.active_environments, **worker))
            .and_then(|(worker, _)| Some((*worker, self.worker_connection(worker)?)))
    }

    /// The live connection of an attached worker advertising `kvm`, least
    /// loaded first.
    #[must_use]
    pub fn kvm_worker(&self, now_ms: u64) -> Option<(DeviceKey, iroh::endpoint::Connection)> {
        self.workers
            .iter()
            .filter(|(worker, advertisement)| {
                advertisement.validate(**worker, now_ms).is_ok()
                    && advertisement
                        .accelerators
                        .contains(&ComputeAccelerator::Kvm)
                    && !advertisement.energy_saver
            })
            .min_by_key(|(worker, advertisement)| (advertisement.active_assignments, **worker))
            .and_then(|(worker, _)| Some((*worker, self.worker_connection(worker)?)))
    }

    /// What the workers attached here run for environments now: a live,
    /// connected worker with environment capacity, or one advertising
    /// `kvm`.
    #[must_use]
    pub fn environment_offer(&self, now_ms: u64) -> tilo_relay_protocol::RelayEnvironments {
        let live = |worker: &DeviceKey, advertisement: &ComputeWorkerAdvertisement| {
            advertisement.validate(*worker, now_ms).is_ok()
                && self
                    .worker_connection(worker)
                    .is_some_and(|connection| connection.close_reason().is_none())
        };
        tilo_relay_protocol::RelayEnvironments {
            sessions: self.workers.iter().any(|(worker, advertisement)| {
                live(worker, advertisement) && advertisement.environment_capacity > 0
            }),
            legacy_apps: self.workers.iter().any(|(worker, advertisement)| {
                live(worker, advertisement)
                    && advertisement
                        .accelerators
                        .contains(&ComputeAccelerator::Kvm)
            }),
        }
    }

    /// The live connection of an attached device.
    #[must_use]
    pub fn worker_connection(&self, worker: &DeviceKey) -> Option<iroh::endpoint::Connection> {
        self.sessions
            .get(worker)
            .and_then(|subscriber| subscriber.connection.clone())
    }

    pub fn detach(&mut self, device: &DeviceKey) {
        self.sessions.remove(device);
        self.workers.remove(device);
        self.latency.remove(device);
        // Its jobs end with it; none of them completes here.
        self.followed.retain(|_, job| job.scope.pair.1 != *device);
    }

    /// Detach the session whose disconnect signal is `disconnect`, unless a
    /// newer session of the same device replaced it. Returns whether the
    /// device's compute state was this session's to release: `false` when
    /// a newer session holds it, which this one must leave alone.
    pub fn detach_session(&mut self, device: &DeviceKey, disconnect: &mpsc::Sender<()>) -> bool {
        if self
            .sessions
            .get(device)
            .is_some_and(|current| !current.disconnect.same_channel(disconnect))
        {
            return false;
        }
        self.detach(device);
        true
    }

    /// End every job still bound to `worker` as
    /// [`ComputeJobPhase::Failed`] with `code` ([`WORKER_LOST`],
    /// [`WORKER_REVOKED`] or [`WORKER_NOT_ADMITTED`]) and tell each
    /// requester now, rather than leaving it to wait out the job's
    /// lifetime. Deliveries still queued to the worker for those jobs are
    /// withdrawn, so a returning worker is not handed work that already
    /// ended. Returns how many jobs ended.
    ///
    /// Blocking: it scans the workspace's job records, so it runs off the
    /// async runtime.
    ///
    /// # Errors
    ///
    /// Returns the first store or routing failure.
    pub fn fail_worker_jobs(
        &mut self,
        store: &WorkspaceStore,
        worker: &DeviceKey,
        code: &'static str,
        next_sequence: &mut dyn FnMut() -> u64,
    ) -> Result<usize, ComputeTransportError> {
        let mut ended = BTreeSet::new();
        for record in store.load_compute_jobs()? {
            if record.spec.worker != *worker || record.lifecycle.phase.is_terminal() {
                continue;
            }
            let job_id = record.spec.job_id;
            self.end_job(store, record, code, None, next_sequence)?;
            self.settle(&job_id, worker, false);
            ended.insert(job_id);
        }
        if !ended.is_empty() {
            withdraw_queued(store, worker, &ended)?;
        }
        Ok(ended.len())
    }

    /// End every job an earlier run of this relay left unfinished as
    /// [`ComputeJobPhase::Failed`] with [`RELAY_RESTARTED`]. Called as the
    /// workspace opens, before any session can attach: each requester is
    /// told through its durable queue, replayed until it acknowledges, and
    /// deliveries still queued to a worker for those jobs are withdrawn.
    ///
    /// Idempotent across restarts: an ended job is skipped, and a job whose
    /// failure reached its requester's queue before the record was saved (a
    /// crash in between) is ended with that queued failure, not told twice.
    /// Returns how many jobs ended.
    ///
    /// Blocking: it scans the workspace's job records and the queues of
    /// their requesters.
    ///
    /// # Errors
    ///
    /// Returns the first store or routing failure.
    pub fn fail_unfinished_jobs(
        &mut self,
        store: &WorkspaceStore,
        next_sequence: &mut dyn FnMut() -> u64,
    ) -> Result<usize, ComputeTransportError> {
        let mut unfinished: Vec<_> = store
            .load_compute_jobs()?
            .into_iter()
            .filter(|record| !record.lifecycle.phase.is_terminal())
            .collect();
        if unfinished.is_empty() {
            return Ok(0);
        }
        // Told in the order the jobs were asked for.
        unfinished.sort_by_key(|record| (record.spec.issued_at_ms, record.spec.job_id));
        let mut told = HashMap::new();
        let requesters: BTreeSet<DeviceKey> = unfinished
            .iter()
            .map(|record| record.spec.requester)
            .collect();
        for requester in &requesters {
            told.extend(queued_results(store, requester)?);
        }
        let mut ended: HashMap<DeviceKey, BTreeSet<[u8; 16]>> = HashMap::new();
        for record in unfinished {
            let (worker, job_id) = (record.spec.worker, record.spec.job_id);
            let queued = told.remove(&job_id);
            self.end_job(store, record, RELAY_RESTARTED, queued, next_sequence)?;
            ended.entry(worker).or_default().insert(job_id);
        }
        for (worker, jobs) in &ended {
            withdraw_queued(store, worker, jobs)?;
        }
        Ok(ended.values().map(BTreeSet::len).sum())
    }

    /// End `record` as [`ComputeJobPhase::Failed`] with `code`: route the
    /// result to its requester, then save the record, so a crash in
    /// between leaves the requester told rather than the job silently
    /// ended. `queued` is a terminal result for the job already in the
    /// requester's queue, which the record then takes instead.
    fn end_job(
        &mut self,
        store: &WorkspaceStore,
        mut record: DurableComputeJob,
        code: &'static str,
        queued: Option<ComputeResultUpdate>,
        next_sequence: &mut dyn FnMut() -> u64,
    ) -> Result<(), ComputeTransportError> {
        let adopted = queued.filter(|result| {
            validate_binding(&result.binding, &record.spec).is_ok()
                && result.phase.is_terminal()
                && record
                    .lifecycle
                    .clone()
                    .advance(result.sequence, result.phase.clone())
                    .is_ok()
        });
        let result = if let Some(result) = adopted {
            result
        } else {
            let result = ComputeResultUpdate {
                binding: ComputeJobBinding {
                    workspace_id: record.spec.workspace_id,
                    job_id: record.spec.job_id,
                    requester: record.spec.requester,
                    worker: record.spec.worker,
                },
                sequence: record.lifecycle.sequence.saturating_add(1),
                phase: ComputeJobPhase::Failed { code: code.into() },
                log_tail: failure_explanation(code).to_vec(),
            };
            self.route(
                record.spec.requester,
                &ComputeEnvelope::Result {
                    result: result.clone(),
                },
                store,
                next_sequence(),
            )?;
            result
        };
        record
            .lifecycle
            .advance(result.sequence, result.phase)
            .map_err(|_| ComputeTransportError::InvalidLifecycle)?;
        record.log_bytes = record
            .log_bytes
            .saturating_add(u64::try_from(result.log_tail.len()).unwrap_or(u64::MAX));
        store.save_compute_job(&record)?;
        Ok(())
    }

    /// Durably enqueue an envelope and attempt immediate delivery when the
    /// target currently has a live session.
    ///
    /// # Errors
    ///
    /// Returns an error when the delivery cannot be encoded within protocol
    /// bounds or persisted to the workspace queue.
    pub fn route(
        &mut self,
        target: DeviceKey,
        envelope: &ComputeEnvelope,
        store: &WorkspaceStore,
        sequence: u64,
    ) -> Result<(), ComputeTransportError> {
        let delivery = ComputeEnvelope::Delivery {
            workspace_id: store.workspace_id(),
            sequence,
            message: Box::new(envelope.clone()),
        };
        let bytes = postcard::to_allocvec(&delivery)
            .map_err(|error| StoreError::Json(format!("encode compute envelope: {error}")))?;
        if bytes.len() > MAX_COMPUTE_FRAME_BYTES {
            return Err(ComputeTransportError::MetadataTooLarge);
        }
        store.enqueue_compute_pending(&target, sequence, &bytes)?;
        if let Some(subscriber) = self.sessions.get(&target) {
            if subscriber.sender.try_send(delivery).is_ok() {
                return Ok(());
            }
            let _ = subscriber.disconnect.try_send(());
            self.detach(&target);
        }
        Ok(())
    }

    /// Validate and durably claim a new compute request.
    ///
    /// # Errors
    ///
    /// Returns an error for forged or conflicting routes, unavailable workers,
    /// invalid job specifications, or durable-store failures.
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
        admit_compute_job(
            spec,
            &worker.server_advertisement(self.latency_ms(&spec.worker)),
            now_ms,
        )
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

    /// Record a cancellation request for an existing job.
    ///
    /// # Errors
    ///
    /// Returns an error when the requester does not own the route, the job is
    /// unknown, its binding is inconsistent, or durable persistence fails.
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

    /// Apply a non-terminal lifecycle update from the job's bound worker.
    ///
    /// # Errors
    ///
    /// Returns an error for forged routes, invalid lifecycle transitions,
    /// oversized metadata, unknown jobs, or durable-store failures.
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
        Self::accept_worker_update(
            authenticated_worker,
            store,
            &update.binding,
            update.sequence,
            update.phase.clone(),
            &update.log_tail,
        )
    }

    /// Apply a terminal result from the job's bound worker. A completed job
    /// teaches placement which of its inputs the worker held.
    ///
    /// # Errors
    ///
    /// Returns an error for forged routes, non-terminal or invalid lifecycle
    /// transitions, oversized metadata, unknown jobs, or store failures.
    pub fn accept_result(
        &mut self,
        authenticated_worker: DeviceKey,
        store: &WorkspaceStore,
        update: &ComputeResultUpdate,
    ) -> Result<ComputeRouteOutcome, ComputeTransportError> {
        if !update.phase.is_terminal() {
            return Err(ComputeTransportError::InvalidLifecycle);
        }
        let outcome = Self::accept_worker_update(
            authenticated_worker,
            store,
            &update.binding,
            update.sequence,
            update.phase.clone(),
            &update.log_tail,
        )?;
        if outcome != ComputeRouteOutcome::Duplicate {
            self.settle(
                &update.binding.job_id,
                &authenticated_worker,
                matches!(update.phase, ComputeJobPhase::Completed { .. }),
            );
        }
        Ok(outcome)
    }

    fn accept_worker_update(
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

/// What the requester is told when its job ends with `code`.
fn failure_explanation(code: &str) -> &'static [u8] {
    match code {
        WORKER_REVOKED => b"the worker's device certificate was revoked while it held the job",
        WORKER_NOT_ADMITTED => b"the worker is no longer admitted to this workspace",
        RELAY_RESTARTED => b"the relay restarted while the job was unfinished; nothing runs it now",
        _ => b"the worker's connection to the relay ended while it held the job",
    }
}

/// The terminal results queued to `requester`, by job.
fn queued_results(
    store: &WorkspaceStore,
    requester: &DeviceKey,
) -> Result<HashMap<[u8; 16], ComputeResultUpdate>, ComputeTransportError> {
    let mut results = HashMap::new();
    for entry in store.load_compute_pending(requester)? {
        let Ok(ComputeEnvelope::Delivery { message, .. }) =
            postcard::from_bytes::<ComputeEnvelope>(&entry.bytes)
        else {
            continue;
        };
        if let ComputeEnvelope::Result { result } = *message
            && result.phase.is_terminal()
        {
            results.insert(result.binding.job_id, result);
        }
    }
    Ok(results)
}

/// Drop deliveries queued to `worker` that concern a job in `ended`.
fn withdraw_queued(
    store: &WorkspaceStore,
    worker: &DeviceKey,
    ended: &BTreeSet<[u8; 16]>,
) -> Result<(), ComputeTransportError> {
    for entry in store.load_compute_pending(worker)? {
        let Ok(ComputeEnvelope::Delivery {
            sequence, message, ..
        }) = postcard::from_bytes::<ComputeEnvelope>(&entry.bytes)
        else {
            continue;
        };
        let job_id = match &*message {
            ComputeEnvelope::Request { spec } => spec.job_id,
            ComputeEnvelope::Cancel { binding } => binding.job_id,
            _ => continue,
        };
        if ended.contains(&job_id) {
            store.ack_compute_pending(worker, sequence)?;
        }
    }
    Ok(())
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
            accelerators: if gpu {
                BTreeSet::from([ComputeAccelerator::Gpu])
            } else {
                BTreeSet::new()
            },
            assignment_capacity: 2,
            active_assignments: 0,
            environment_capacity: 0,
            active_environments: 0,
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
    fn placement_names_a_worker_the_relay_then_admits_the_job_for() {
        let root = tempfile::tempdir().unwrap();
        let store = open_store(root.path());
        let mut state = ComputeTransportState::default();
        let needs = spec();
        assert!(
            state
                .place(&needs.requirements(), 200)
                .unwrap_err()
                .considered
                .is_empty(),
            "nothing advertised, nothing placed"
        );

        // A busy worker on battery and an idle one on external power.
        let mut laptop = advertisement(false);
        laptop.worker = [5; 32];
        laptop.power = PowerAvailability::Battery(60);
        laptop.active_assignments = 1;
        state.advertise([5; 32], 7, 200, laptop).unwrap();
        state
            .advertise([2; 32], 7, 200, advertisement(false))
            .unwrap();
        let placement = state.place(&needs.requirements(), 200).unwrap();
        assert_eq!(placement.worker, [2; 32]);
        let mut placed = needs.clone();
        placed.worker = placement.worker;
        assert_eq!(
            state
                .accept_request([1; 32], 7, 200, &store, &placed)
                .unwrap(),
            ComputeRouteOutcome::Route([2; 32])
        );

        // A GPU job is passed over by both, with the reason; once the
        // advertisements expire, every worker is offline.
        let mut gpu = needs.clone();
        gpu.limits.gpu = true;
        let refused = state.place(&gpu.requirements(), 200).unwrap_err();
        assert!(
            refused
                .considered
                .iter()
                .all(|candidate| candidate.verdict == Err(crate::placement::Ineligible::NoGpu)),
            "{refused}"
        );
        let expired = state.place(&needs.requirements(), 10_000).unwrap_err();
        assert!(
            expired
                .considered
                .iter()
                .all(|candidate| candidate.verdict == Err(crate::placement::Ineligible::Offline)),
            "{expired}"
        );
    }

    #[test]
    fn placement_prefers_the_lower_latency_worker_among_equals() {
        let mut state = ComputeTransportState::default();
        let needs = spec();
        let mut far = advertisement(false);
        far.worker = [2; 32];
        let mut near = advertisement(false);
        near.worker = [5; 32];
        state.advertise([2; 32], 7, 200, far).unwrap();
        state.advertise([5; 32], 7, 200, near).unwrap();
        assert_eq!(state.latency_ms(&[5; 32]), UNMEASURED_LATENCY_MS);

        // Unmeasured, equal workers fall through to the device key; a
        // measured worker beats one the relay has not observed yet.
        assert_eq!(
            state.place(&needs.requirements(), 200).unwrap().worker,
            [2; 32]
        );
        state.observe_latency([2; 32], Duration::from_millis(40));
        let placement = state.place(&needs.requirements(), 200).unwrap();
        assert_eq!((placement.worker, placement.rank.latency_ms), ([2; 32], 40));

        // Equal priority, power and load: the closer worker wins.
        state.observe_latency([5; 32], Duration::from_millis(5));
        let placement = state.place(&needs.requirements(), 200).unwrap();
        assert_eq!((placement.worker, placement.rank.latency_ms), ([5; 32], 5));
        assert!(
            placement.to_string().contains("latency 5 ms"),
            "{placement}"
        );

        // One stray sample moves the estimate a quarter of the way, not all
        // of it; sustained samples converge.
        state.observe_latency([5; 32], Duration::from_millis(405));
        assert_eq!(state.latency_ms(&[5; 32]), 105);
        for _ in 0..64 {
            state.observe_latency([5; 32], Duration::from_millis(80));
        }
        assert_eq!(state.latency_ms(&[5; 32]), 80);
        assert_eq!(
            state.place(&needs.requirements(), 200).unwrap().worker,
            [2; 32]
        );

        // A reattached worker starts unmeasured again, and a sub-millisecond
        // LAN round trip on its new connection still reads as measured.
        let (sender, _receiver) = mpsc::channel(1);
        let (disconnect, _disconnected) = mpsc::channel(1);
        state.attach([2; 32], sender, disconnect, None);
        assert_eq!(state.latency_ms(&[2; 32]), UNMEASURED_LATENCY_MS);
        state.observe_latency([2; 32], Duration::from_micros(300));
        assert_eq!(state.latency_ms(&[2; 32]), 1);
    }

    /// Moving 129 MB at a measured 38 MB/s takes longer than the 507 ms the
    /// requester's own run took, so the job stays with it, with the
    /// estimate; a small batch of heavy compute still goes to the worker,
    /// and a worker whose rate is not measured yet is not held back.
    #[test]
    fn placement_weighs_moving_the_inputs_against_the_requesters_own_run() {
        const MB: u64 = 1_000_000;
        let mut state = ComputeTransportState::default();
        let needs = spec();
        state
            .advertise([2; 32], 7, 200, advertisement(false))
            .unwrap();
        state.observe_latency([2; 32], Duration::from_millis(2));
        // Three loopback-rate transfers of 10.75 MB each, as the 12-image
        // batch moved; small transfers say nothing about the rate.
        for _ in 0..3 {
            state.observe_transfer(([1; 32], [2; 32]), 10_750_000, Duration::from_millis(283));
        }
        state.observe_transfer(([1; 32], [2; 32]), 1_000, Duration::from_secs(10));
        let rate = state.throughput(&[1; 32], &[2; 32]).unwrap();
        assert!((37 * MB..39 * MB).contains(&rate), "{rate} bytes/s");

        let heavy = InputWeight {
            input_bytes: 129 * MB,
            local_wall_ms: 507,
            inputs: Vec::new(),
        };
        let WeighedPlacement::Local(kept) = state
            .place_weighed(&needs.requirements(), Some(&heavy), 200)
            .unwrap()
        else {
            panic!("129 MB on a 38 MB/s link is slower than 507 ms here");
        };
        assert_eq!(kept.worker, [2; 32]);
        assert!((3_300..3_500).contains(&kept.transfer_ms), "{kept}");
        assert_eq!(kept.local_wall_ms, 507);
        assert!(
            kept.to_string().starts_with(INPUTS_HEAVIER_THAN_GAIN),
            "{kept}"
        );

        // Small inputs, heavy compute: the worker.
        let light = InputWeight {
            input_bytes: 2 * MB,
            local_wall_ms: 20_000,
            inputs: Vec::new(),
        };
        assert!(matches!(
            state.place_weighed(&needs.requirements(), Some(&light), 200).unwrap(),
            WeighedPlacement::Worker(placement) if placement.worker == [2; 32]
        ));
        // Without a weight, placement is unchanged.
        assert!(matches!(
            state.place_weighed(&needs.requirements(), None, 200).unwrap(),
            WeighedPlacement::Worker(placement) if placement.worker == [2; 32]
        ));

        // A second worker, ranked lower (on battery) and never measured,
        // takes the heavy job rather than keeping it here: offloading to it
        // is how its rate gets known.
        let mut laptop = advertisement(false);
        laptop.worker = [5; 32];
        laptop.power = PowerAvailability::Battery(80);
        state.advertise([5; 32], 7, 200, laptop).unwrap();
        let WeighedPlacement::Worker(placement) = state
            .place_weighed(&needs.requirements(), Some(&heavy), 200)
            .unwrap()
        else {
            panic!("an unmeasured worker is not held back");
        };
        assert_eq!(placement.worker, [5; 32]);
        assert_eq!(placement.rank.power, crate::placement::PowerClass::Battery);

        // A reattached worker's rate is unknown again.
        let (sender, _receiver) = mpsc::channel(1);
        let (disconnect, _disconnected) = mpsc::channel(1);
        state.attach([2; 32], sender, disconnect, None);
        assert_eq!(state.throughput(&[1; 32], &[2; 32]), None);
    }

    /// Run `job` to `phase` on its worker, as the worker reports it.
    fn finish(
        state: &mut ComputeTransportState,
        store: &WorkspaceStore,
        job: &ComputeJobSpec,
        phase: ComputeJobPhase,
    ) {
        let binding = ComputeJobBinding {
            workspace_id: job.workspace_id,
            job_id: job.job_id,
            requester: job.requester,
            worker: job.worker,
        };
        state
            .accept_status(
                job.worker,
                store,
                &ComputeStatusUpdate {
                    binding: binding.clone(),
                    sequence: 1,
                    phase: ComputeJobPhase::Running,
                    log_tail: Vec::new(),
                },
            )
            .unwrap();
        state
            .accept_result(
                job.worker,
                store,
                &ComputeResultUpdate {
                    binding,
                    sequence: 2,
                    phase,
                    log_tail: Vec::new(),
                },
            )
            .unwrap();
    }

    /// The thumbnail batch: twelve camera originals of 10.75 MB.
    fn batch(job_id: u8, first_photo: u8) -> (ComputeJobSpec, Vec<([u8; 32], u64)>) {
        let photos = (first_photo..first_photo.saturating_add(12))
            .map(|seed| ([seed; 32], 10_750_000))
            .collect::<Vec<_>>();
        let mut job = spec();
        job.job_id = [job_id; 16];
        job.inputs = photos.iter().map(|(digest, _)| *digest).collect();
        (job, photos)
    }

    fn scope(job: &ComputeJobSpec) -> JobScope {
        JobScope {
            job_id: job.job_id,
            pair: (job.requester, job.worker),
            expires_at_ms: job.expires_at_ms,
        }
    }

    /// Weighing the batch against the requester's own 507 ms run.
    fn weigh(
        state: &mut ComputeTransportState,
        photos: &[([u8; 32], u64)],
        listed: bool,
    ) -> WeighedPlacement {
        let weight = InputWeight {
            input_bytes: photos.iter().map(|(_, bytes)| bytes).sum(),
            local_wall_ms: 507,
            inputs: if listed { photos.to_vec() } else { Vec::new() },
        };
        state
            .place_weighed(&spec().requirements(), Some(&weight), 200)
            .unwrap()
    }

    /// Over a slow measured link (a phone's 1 MB/s uplink), the 129 MB
    /// thumbnail batch stays on the phone while the relay knows nothing of
    /// what the worker holds, exactly as before. Once a completed job shows
    /// the worker staged its own proven copies, the batch goes to it, and
    /// so does the next batch of new photographs; once a job shows it
    /// fetched them after all (its copies gone or not matching), placement
    /// is back to weighing every byte. None of it is the worker's word.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one worker's holdings learned, used and unlearned, top to bottom"
    )]
    fn placement_discounts_inputs_the_worker_was_seen_to_hold() {
        const MB: u64 = 1_000_000;
        let root = tempfile::tempdir().unwrap();
        let (mut state, store) = admitted(root.path());
        state.observe_latency([2; 32], Duration::from_millis(30));
        state.observe_transfer(
            ([1; 32], [2; 32]),
            10_750_000,
            Duration::from_millis(10_750),
        );
        assert_eq!(state.throughput(&[1; 32], &[2; 32]), Some(MB));

        // Nothing known: every byte is weighed, as before.
        let (held_run, photos) = batch(20, 10);
        for listed in [false, true] {
            let WeighedPlacement::Local(kept) = weigh(&mut state, &photos, listed) else {
                panic!("129 MB at 1 MB/s is slower than 507 ms");
            };
            assert_eq!((kept.input_bytes, kept.held_bytes), (129 * MB, 0), "{kept}");
            assert!(kept.transfer_ms > 129_000, "{kept}");
        }

        // A job on those photographs completes with the phone having proved
        // each and the relay forwarding none: the worker held them all.
        state
            .accept_request([1; 32], 7, 200, &store, &held_run)
            .unwrap();
        for (digest, _) in &photos {
            state.observe_proof(scope(&held_run), *digest, 200);
        }
        finish(
            &mut state,
            &store,
            &held_run,
            ComputeJobPhase::Completed {
                outputs: vec![[7; 32]],
            },
        );
        assert!(state.followed.is_empty(), "a settled job is not followed");
        for listed in [true, false] {
            let WeighedPlacement::Worker(placement) = weigh(&mut state, &photos, listed) else {
                panic!("nothing moves to a worker holding every input");
            };
            assert_eq!(placement.worker, [2; 32]);
        }
        // Moving nothing still costs the round trip: a job the phone runs
        // faster than that stays on the phone.
        let quick = InputWeight {
            input_bytes: 129 * MB,
            local_wall_ms: 20,
            inputs: photos.clone(),
        };
        let WeighedPlacement::Local(kept) = state
            .place_weighed(&spec().requirements(), Some(&quick), 200)
            .unwrap()
        else {
            panic!("a 30 ms round trip is slower than a 20 ms run");
        };
        assert_eq!(
            (kept.input_bytes, kept.held_bytes, kept.transfer_ms),
            (0, 129 * MB, 30),
            "{kept}"
        );
        // The next batch is new photographs, never seen: a worker that held
        // all of its last job's inputs is expected to hold these too.
        let (_, new_photos) = batch(21, 30);
        assert!(matches!(
            weigh(&mut state, &new_photos, true),
            WeighedPlacement::Worker(placement) if placement.worker == [2; 32]
        ));

        // The hint goes wrong: the worker no longer holds the photographs.
        // The job placed on it proves them, fetches them anyway (its copies
        // gone or not matching), and still completes on the phone's bytes;
        // it cost a slow run, and the relay weighs every byte again.
        let (fetched_run, _) = batch(22, 10);
        state
            .accept_request([1; 32], 7, 200, &store, &fetched_run)
            .unwrap();
        for (digest, _) in &photos {
            state.observe_proof(scope(&fetched_run), *digest, 200);
            state.observe_forwarded(scope(&fetched_run), *digest, 200);
        }
        finish(
            &mut state,
            &store,
            &fetched_run,
            ComputeJobPhase::Completed {
                outputs: vec![[7; 32]],
            },
        );
        for (batch_photos, listed) in [(&photos, true), (&photos, false), (&new_photos, true)] {
            let WeighedPlacement::Local(kept) = weigh(&mut state, batch_photos, listed) else {
                panic!("a worker seen fetching its inputs is weighed as receiving them");
            };
            assert_eq!((kept.input_bytes, kept.held_bytes), (129 * MB, 0), "{kept}");
        }

        // Half held: only the half it fetched is weighed.
        let (half_run, _) = batch(23, 10);
        state
            .accept_request([1; 32], 7, 200, &store, &half_run)
            .unwrap();
        for (index, (digest, _)) in photos.iter().enumerate() {
            state.observe_proof(scope(&half_run), *digest, 200);
            if index % 2 == 1 {
                state.observe_forwarded(scope(&half_run), *digest, 200);
            }
        }
        finish(
            &mut state,
            &store,
            &half_run,
            ComputeJobPhase::Completed {
                outputs: vec![[7; 32]],
            },
        );
        let WeighedPlacement::Local(kept) = weigh(&mut state, &photos, true) else {
            panic!("64.5 MB at 1 MB/s is still slower than 507 ms");
        };
        assert_eq!(
            (kept.input_bytes, kept.held_bytes),
            (64_500_000, 64_500_000),
            "{kept}"
        );

        // A job that fails teaches nothing: these new photographs are only
        // expected half held, as the last completed job was.
        let (failed_run, other_photos) = batch(24, 50);
        state
            .accept_request([1; 32], 7, 200, &store, &failed_run)
            .unwrap();
        for (digest, _) in &other_photos {
            state.observe_proof(scope(&failed_run), *digest, 200);
        }
        finish(
            &mut state,
            &store,
            &failed_run,
            ComputeJobPhase::Failed {
                code: "input_unavailable".into(),
            },
        );
        assert!(matches!(
            weigh(&mut state, &other_photos, true),
            WeighedPlacement::Local(kept) if kept.input_bytes == 64_500_000
        ));

        // A worker that reattaches may have restarted on other storage:
        // what it was seen to hold is unknown again.
        let (sender, _receiver) = mpsc::channel(1);
        let (disconnect, _disconnected) = mpsc::channel(1);
        state.attach([2; 32], sender, disconnect, None);
        assert!(state.holdings.is_empty());
    }

    /// The thumbnail model of the offloadable-action measurements: one
    /// batch of twelve new 10.75 MB camera originals a minute, which the
    /// phone runs in 507 ms, against a home server 30 ms away whose last job
    /// fetched every input. `holds_from` is the minute its replica has
    /// synced the photographs, if ever.
    struct ThumbnailDay {
        state: ComputeTransportState,
        store: WorkspaceStore,
        _root: tempfile::TempDir,
        holds_from: Option<u64>,
    }

    /// Where each batch ran, and what the re-probes among them cost.
    #[derive(Debug, Default)]
    struct Decisions {
        local: u64,
        worker: u64,
        /// The minutes a re-probe placed a batch on the worker.
        reprobes: Vec<u64>,
        /// What re-probes that found nothing held cost beyond the local
        /// run, by the estimate that had kept the batch local.
        wasted_ms: u64,
        /// What the batches kept local would have saved on a worker
        /// holding their inputs.
        forgone_ms: u64,
    }

    const LOCAL_WALL_MS: u64 = 507;
    /// What a batch gains on a worker holding its inputs: the run less the
    /// 30 ms round trip.
    const GAIN_MS: u64 = 477;

    /// `offset_ms` into minute `minute` of the simulated day.
    fn at(minute: u64, offset_ms: u64) -> u64 {
        minute.saturating_mul(60_000).saturating_add(offset_ms)
    }

    impl ThumbnailDay {
        /// The worker reached at `bytes_per_second` after a job that
        /// fetched all twelve photographs.
        fn new(bytes_per_second: u64, holds_from: Option<u64>) -> Self {
            let root = tempfile::tempdir().unwrap();
            let (mut state, store) = admitted(root.path());
            state.observe_latency([2; 32], Duration::from_millis(30));
            state.observe_transfer(
                ([1; 32], [2; 32]),
                10_750_000,
                Duration::from_micros(
                    10_750_000_000_000_u64
                        .checked_div(bytes_per_second)
                        .unwrap(),
                ),
            );
            let mut day = Self {
                state,
                store,
                _root: root,
                holds_from,
            };
            day.run(0, false);
            day
        }

        fn photos(minute: u64) -> Vec<([u8; 32], u64)> {
            (1_u8..=12)
                .map(|index| {
                    let mut digest = [index; 32];
                    digest[..8].copy_from_slice(&minute.to_le_bytes());
                    (digest, 10_750_000)
                })
                .collect()
        }

        /// Run minute `minute`'s batch on the worker, which holds the
        /// photographs or fetches every one.
        fn run(&mut self, minute: u64, held: bool) {
            let now = at(minute, 200);
            let mut job = spec();
            job.job_id[..8].copy_from_slice(&minute.to_le_bytes());
            job.inputs = Self::photos(minute)
                .iter()
                .map(|(digest, _)| *digest)
                .collect();
            job.issued_at_ms = now;
            job.expires_at_ms = now.saturating_add(10_000);
            self.advertise(now);
            self.state
                .accept_request([1; 32], 7, now, &self.store, &job)
                .unwrap();
            for digest in &job.inputs {
                self.state.observe_proof(scope(&job), *digest, now);
                if !held {
                    self.state.observe_forwarded(scope(&job), *digest, now);
                }
            }
            finish(
                &mut self.state,
                &self.store,
                &job,
                ComputeJobPhase::Completed {
                    outputs: vec![[7; 32]],
                },
            );
        }

        fn advertise(&mut self, now: u64) {
            let mut fresh = advertisement(false);
            fresh.issued_at_ms = now;
            fresh.expires_at_ms = now.saturating_add(MAX_COMPUTE_ADVERTISEMENT_LIFETIME_MS);
            self.state.advertise([2; 32], 7, now, fresh).unwrap();
        }

        /// Place minute `minute`'s batch, and run it where it was placed.
        fn place(&mut self, minute: u64) -> WeighedPlacement {
            let now = at(minute, 100);
            self.advertise(now);
            let photos = Self::photos(minute);
            let weight = InputWeight {
                input_bytes: photos.iter().map(|(_, bytes)| bytes).sum(),
                local_wall_ms: LOCAL_WALL_MS,
                inputs: photos,
            };
            let placed = self
                .state
                .place_weighed(&spec().requirements(), Some(&weight), now)
                .unwrap();
            if !matches!(placed, WeighedPlacement::Local(_)) {
                self.run(minute, self.holds_from.is_some_and(|from| minute >= from));
            }
            placed
        }

        /// Place a batch every minute through `minutes`.
        fn decide(&mut self, minutes: std::ops::RangeInclusive<u64>) -> Decisions {
            let mut decisions = Decisions::default();
            for minute in minutes {
                let held = self.holds_from.is_some_and(|from| minute >= from);
                match self.place(minute) {
                    WeighedPlacement::Local(kept) => {
                        decisions.local = decisions.local.saturating_add(1);
                        decisions.forgone_ms = decisions.forgone_ms.saturating_add(GAIN_MS);
                        assert_eq!(kept.worker, [2; 32]);
                    }
                    WeighedPlacement::Worker(placement) => {
                        decisions.worker = decisions.worker.saturating_add(1);
                        assert_eq!(placement.worker, [2; 32]);
                    }
                    WeighedPlacement::Reprobe(reprobe) => {
                        decisions.reprobes.push(minute);
                        assert_eq!(reprobe.placement.worker, [2; 32]);
                        assert!(reprobe.to_string().starts_with(REPROBING_HELD_INPUTS));
                        if !held {
                            decisions.wasted_ms = decisions.wasted_ms.saturating_add(
                                reprobe.estimate.transfer_ms.saturating_sub(LOCAL_WALL_MS),
                            );
                        }
                    }
                }
            }
            decisions
        }
    }

    const PHONE_UPLINK: u64 = 1_000_000;
    /// The loopback rate the 12-image batch moved at.
    const LOOPBACK: u64 = 37_985_865;

    /// A worker whose last job fetched every input over a slow link is
    /// kept from the next batches, and tried again once the window has
    /// passed and the batches kept local forwent what a fetched run would
    /// cost. Until that try settles none other is made; the try finding
    /// the photographs held restores placement on the worker.
    #[test]
    fn a_worker_kept_local_on_a_stale_fetch_is_tried_again() {
        for (rate, synced) in [(LOOPBACK, 45), (PHONE_UPLINK, 45)] {
            let mut day = ThumbnailDay::new(rate, Some(synced));
            // Within the window the batch stays, however much it forwent.
            let early = day.decide(1..=9);
            assert_eq!((early.local, early.worker), (9, 0), "{early:?}");
            assert!(early.reprobes.is_empty(), "{early:?}");

            let late = day.decide(10..=300);
            let first = late.reprobes[0];
            if rate == LOOPBACK {
                // 2.9 s of fetching is covered by seven batches; the window,
                // from the first batch kept, is what waits. The tries before
                // the replica synced still fetch, so each next one waits
                // twice as long; the third finds the photographs held.
                assert_eq!(late.reprobes, [11, 32, 73], "{late:?}");
                assert_eq!(late.wasted_ms, 5_840, "{late:?}");
            } else {
                // 128.5 s of fetching at 1 MB/s needs 270 batches' 477 ms.
                assert_eq!(late.reprobes, [270], "{late:?}");
            }
            assert!(at(first, 0) >= REPROBE_AFTER_MS, "{late:?}");
            let restored = *late.reprobes.last().unwrap();
            assert!(restored >= synced);
            // Every batch after the try that found them held runs there.
            assert_eq!(late.worker, 300_u64.saturating_sub(restored), "{late:?}");
            assert_eq!(
                late.local,
                291_u64
                    .saturating_sub(u64::try_from(late.reprobes.len()).unwrap())
                    .saturating_sub(late.worker),
                "{late:?}"
            );
            eprintln!(
                "{} B/s, synced at minute {synced}: re-probed at minutes {:?}, then {} of the \
                 remaining batches on the worker; {} ms wasted on fruitless tries",
                rate, late.reprobes, late.worker, late.wasted_ms
            );
        }

        // A try placed and not yet settled does not repeat: the next batch
        // stays local, and the one after.
        let mut day = ThumbnailDay::new(LOOPBACK, None);
        day.decide(1..=10);
        let photos = ThumbnailDay::photos(11);
        let weight = InputWeight {
            input_bytes: photos.iter().map(|(_, bytes)| bytes).sum(),
            local_wall_ms: LOCAL_WALL_MS,
            inputs: photos,
        };
        let now = at(11, 100);
        day.advertise(now);
        let placed = |day: &mut ThumbnailDay, at| {
            day.state
                .place_weighed(&spec().requirements(), Some(&weight), at)
                .unwrap()
        };
        assert!(matches!(
            placed(&mut day, now),
            WeighedPlacement::Reprobe(_)
        ));
        for later in [now.saturating_add(1), now.saturating_add(30_000)] {
            assert!(matches!(
                placed(&mut day, later),
                WeighedPlacement::Local(_)
            ));
        }

        // Only a pair the relay carried content between is kept: placing
        // for a requester it never measured adds nothing.
        let mut stranger = spec();
        stranger.requester = [8; 32];
        assert!(matches!(
            day.state
                .place_weighed(&stranger.requirements(), Some(&weight), now)
                .unwrap(),
            WeighedPlacement::Worker(_)
        ));
        assert_eq!(day.state.holdings.len(), 1);
    }

    /// A worker that still holds nothing goes back to staying local: each
    /// fruitless try doubles the wait and the forgone gain the next needs,
    /// so a week of batches tries it a handful of times, and the time those
    /// tries cost stays a small share of what staying local forwent.
    #[test]
    fn a_worker_that_still_holds_nothing_is_tried_ever_less_often() {
        const WEEK: u64 = 7 * 24 * 60;
        /// The longest wait between tries, in minutes.
        const CAP: u64 = (REPROBE_AFTER_MS / 60_000) << MAX_REPROBE_BACKOFF;
        for rate in [LOOPBACK, PHONE_UPLINK] {
            let mut day = ThumbnailDay::new(rate, None);
            let week = day.decide(1..=WEEK);
            assert_eq!(week.worker, 0, "{week:?}");
            let tries = u64::try_from(week.reprobes.len()).unwrap();
            assert_eq!(week.local.saturating_add(tries), WEEK);
            let gaps = std::iter::once(week.reprobes[0])
                .chain(
                    week.reprobes
                        .windows(2)
                        .map(|pair| pair[1].saturating_sub(pair[0])),
                )
                .collect::<Vec<_>>();
            // Each gap about doubles until the backoff is capped.
            for pair in gaps.windows(2) {
                let doubled = pair[0].saturating_mul(19).min(CAP.saturating_mul(10));
                assert!(pair[1].saturating_mul(10) >= doubled, "{gaps:?}");
            }
            if rate == LOOPBACK {
                // The window waits (each counted from the first batch kept
                // after a try), until capped at 640 minutes.
                assert_eq!(&gaps[..8], [11, 21, 41, 81, 161, 321, 641, 641], "{gaps:?}");
                assert_eq!(tries, 20, "{gaps:?}");
                assert_eq!(week.wasted_ms, 58_400, "{week:?}");
            } else {
                // The forgone gain waits: 270 batches of 477 ms cover one
                // 128.5 s fetched run, then twice that, and so on.
                assert_eq!(gaps, [270, 539, 1_078, 2_156, 4_312], "{gaps:?}");
                assert_eq!(week.wasted_ms, 642_615, "{week:?}");
            }
            // What the tries cost is bounded by what staying local forwent.
            assert!(
                week.wasted_ms.saturating_mul(5) < week.forgone_ms,
                "{week:?}"
            );
            eprintln!(
                "{rate} B/s, never held: {} of {WEEK} batches local, re-probed at minutes {:?}; \
                 {} ms wasted against {} ms forgone",
                week.local, week.reprobes, week.wasted_ms, week.forgone_ms
            );
        }
    }

    /// What a worker was seen to hold is bounded per requester: the input
    /// learned first is forgotten first.
    #[test]
    fn known_holdings_stay_bounded() {
        let mut holdings = Holdings::default();
        let digest = |index: usize| {
            let mut digest = [0; 32];
            digest[..8].copy_from_slice(&u64::try_from(index).unwrap().to_le_bytes());
            digest
        };
        for index in 0..=MAX_KNOWN_HOLDINGS {
            holdings.learn(digest(index), true);
        }
        holdings.learn(digest(MAX_KNOWN_HOLDINGS), false);
        assert_eq!(holdings.known.len(), MAX_KNOWN_HOLDINGS);
        assert_eq!(holdings.order.len(), MAX_KNOWN_HOLDINGS);
        assert!(!holdings.known.contains_key(&digest(0)));
        assert_eq!(holdings.known.get(&digest(1)), Some(&true));
        assert_eq!(
            holdings.known.get(&digest(MAX_KNOWN_HOLDINGS)),
            Some(&false)
        );

        // So are the live jobs followed: past the bound a new job is not,
        // until expired ones make room.
        let mut state = ComputeTransportState::default();
        let job = |index: usize| JobScope {
            job_id: digest(index)[..16].try_into().unwrap(),
            pair: ([1; 32], [2; 32]),
            expires_at_ms: 10_000,
        };
        for index in 0..=MAX_FOLLOWED_JOBS {
            state.observe_proof(job(index), [4; 32], 200);
        }
        assert_eq!(state.followed.len(), MAX_FOLLOWED_JOBS);
        assert!(!state.followed.contains_key(&job(MAX_FOLLOWED_JOBS).job_id));
        state.observe_proof(job(MAX_FOLLOWED_JOBS), [4; 32], 10_000);
        assert_eq!(state.followed.len(), 1);
    }

    /// A worker that leaves mid-job fails each job it held, Pending or
    /// Running, with the truthful code; its requester hears at once and
    /// the worker is not handed those jobs again.
    #[test]
    fn a_departed_workers_jobs_fail_at_once_and_reach_their_requester() {
        let root = tempfile::tempdir().unwrap();
        let (mut state, store) = admitted(root.path());
        state
            .route(
                [2; 32],
                &ComputeEnvelope::Request { spec: spec() },
                &store,
                1,
            )
            .unwrap();
        state
            .accept_status(
                [2; 32],
                &store,
                &ComputeStatusUpdate {
                    binding: binding(),
                    sequence: 1,
                    phase: ComputeJobPhase::Running,
                    log_tail: Vec::new(),
                },
            )
            .unwrap();
        // A second job, still Pending, and another worker's job.
        let mut pending = spec();
        pending.job_id = [10; 16];
        state
            .accept_request([1; 32], 7, 200, &store, &pending)
            .unwrap();
        state
            .route(
                [2; 32],
                &ComputeEnvelope::Request { spec: pending },
                &store,
                2,
            )
            .unwrap();
        let mut elsewhere = advertisement(false);
        elsewhere.worker = [5; 32];
        state.advertise([5; 32], 7, 200, elsewhere).unwrap();
        let mut other = spec();
        other.job_id = [11; 16];
        other.worker = [5; 32];
        state
            .accept_request([1; 32], 7, 200, &store, &other)
            .unwrap();

        // The worker is gone; its requester is attached.
        state.detach(&[2; 32]);
        let (requester, mut delivered) = mpsc::channel(4);
        let (requester_disconnect, _requester_disconnected) = mpsc::channel(1);
        state.attach([1; 32], requester, requester_disconnect, None);

        let mut sequence = 100;
        let mut next = || {
            sequence += 1;
            sequence
        };
        assert_eq!(
            state
                .fail_worker_jobs(&store, &[2; 32], WORKER_LOST, &mut next)
                .unwrap(),
            2
        );
        let lost = ComputeJobPhase::Failed {
            code: WORKER_LOST.into(),
        };
        for (job_id, sequence) in [([9; 16], 2), ([10; 16], 1)] {
            let record = store.load_compute_job(&job_id).unwrap().unwrap().lifecycle;
            assert_eq!((record.phase, record.sequence), (lost.clone(), sequence));
        }
        let mut told = Vec::new();
        while let Ok(ComputeEnvelope::Delivery { message, .. }) = delivered.try_recv() {
            let ComputeEnvelope::Result { result } = *message else {
                panic!("the requester is told the result");
            };
            assert_eq!(result.phase, lost);
            assert!(!result.log_tail.is_empty());
            told.push(result.binding.job_id);
        }
        told.sort_unstable();
        assert_eq!(told, vec![[9; 16], [10; 16]]);
        assert!(
            store.load_compute_pending(&[2; 32]).unwrap().is_empty(),
            "a returning worker is not handed jobs that already ended"
        );
        // The other worker's job is untouched, and ending again is a no-op.
        assert_eq!(
            store
                .load_compute_job(&[11; 16])
                .unwrap()
                .unwrap()
                .lifecycle
                .phase,
            ComputeJobPhase::Pending
        );
        assert_eq!(
            state
                .fail_worker_jobs(&store, &[2; 32], WORKER_REVOKED, &mut next)
                .unwrap(),
            0
        );
    }

    /// The failure results queued to `device`, by job, with their queue
    /// sequences.
    fn queued_failures(
        store: &WorkspaceStore,
        device: &DeviceKey,
    ) -> Vec<([u8; 16], u64, ComputeResultUpdate)> {
        let mut failures = Vec::new();
        for entry in store.load_compute_pending(device).unwrap() {
            let ComputeEnvelope::Delivery {
                sequence, message, ..
            } = postcard::from_bytes::<ComputeEnvelope>(&entry.bytes).unwrap()
            else {
                panic!("the queue holds deliveries");
            };
            if let ComputeEnvelope::Result { result } = *message {
                failures.push((result.binding.job_id, sequence, result));
            }
        }
        failures.sort_by_key(|(job_id, ..)| *job_id);
        failures
    }

    /// A relay that comes back after a crash ends every job it left
    /// unfinished, Pending or Running, as `relay_restarted`: each requester
    /// is told once, durably, the worker is not handed those jobs, and an
    /// ended job is left as it was. Coming back again tells no one twice.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one crash and two restarts, each job's end checked in order"
    )]
    fn a_restarted_relay_ends_every_unfinished_job_once() {
        let root = tempfile::tempdir().unwrap();
        let (mut state, store) = admitted(root.path());
        state
            .accept_status(
                [2; 32],
                &store,
                &ComputeStatusUpdate {
                    binding: binding(),
                    sequence: 1,
                    phase: ComputeJobPhase::Running,
                    log_tail: Vec::new(),
                },
            )
            .unwrap();
        // A second job, still Pending, its request still queued to the
        // worker.
        let mut pending = spec();
        pending.job_id = [10; 16];
        state
            .accept_request([1; 32], 7, 200, &store, &pending)
            .unwrap();
        state
            .route(
                [2; 32],
                &ComputeEnvelope::Request { spec: pending },
                &store,
                2,
            )
            .unwrap();
        // A third job that completed before the crash.
        let mut done = spec();
        done.job_id = [11; 16];
        state
            .accept_request([1; 32], 7, 200, &store, &done)
            .unwrap();
        let done_binding = ComputeJobBinding {
            job_id: [11; 16],
            ..binding()
        };
        state
            .accept_status(
                [2; 32],
                &store,
                &ComputeStatusUpdate {
                    binding: done_binding.clone(),
                    sequence: 1,
                    phase: ComputeJobPhase::Running,
                    log_tail: Vec::new(),
                },
            )
            .unwrap();
        state
            .accept_result(
                [2; 32],
                &store,
                &ComputeResultUpdate {
                    binding: done_binding,
                    sequence: 2,
                    phase: ComputeJobPhase::Completed {
                        outputs: vec![[5; 32]],
                    },
                    log_tail: Vec::new(),
                },
            )
            .unwrap();

        // The relay comes back: a fresh state over the same store.
        let mut restarted = ComputeTransportState::default();
        let mut sequence = 100;
        let mut next = || {
            sequence += 1;
            sequence
        };
        assert_eq!(
            restarted.fail_unfinished_jobs(&store, &mut next).unwrap(),
            2
        );
        let restart = ComputeJobPhase::Failed {
            code: RELAY_RESTARTED.into(),
        };
        for (job_id, sequence) in [([9; 16], 2), ([10; 16], 1)] {
            let record = store.load_compute_job(&job_id).unwrap().unwrap().lifecycle;
            assert_eq!((record.phase, record.sequence), (restart.clone(), sequence));
        }
        assert!(matches!(
            store
                .load_compute_job(&[11; 16])
                .unwrap()
                .unwrap()
                .lifecycle
                .phase,
            ComputeJobPhase::Completed { .. }
        ));
        let told = queued_failures(&store, &[1; 32]);
        assert_eq!(
            told.iter()
                .map(|(job_id, _, result)| (*job_id, result.sequence, result.phase.clone()))
                .collect::<Vec<_>>(),
            vec![([9; 16], 2, restart.clone()), ([10; 16], 1, restart)]
        );
        assert!(told.iter().all(|(.., result)| !result.log_tail.is_empty()));
        assert!(
            store.load_compute_pending(&[2; 32]).unwrap().is_empty(),
            "the returning worker is not handed a job that already ended"
        );

        // Again: nothing is unfinished, and no one is told twice.
        let mut again = ComputeTransportState::default();
        assert_eq!(again.fail_unfinished_jobs(&store, &mut next).unwrap(), 0);
        assert_eq!(queued_failures(&store, &[1; 32]), told);
    }

    /// A crash after a requester was told but before the job's record was
    /// saved leaves the job unfinished on disk; the next start ends it with
    /// the failure already queued rather than telling the requester twice.
    #[test]
    fn a_crash_between_telling_and_saving_tells_the_requester_once() {
        let root = tempfile::tempdir().unwrap();
        let (_, store) = admitted(root.path());
        let before = store.load_compute_job(&[9; 16]).unwrap().unwrap();
        let mut sequence = 100;
        let mut next = || {
            sequence += 1;
            sequence
        };
        assert_eq!(
            ComputeTransportState::default()
                .fail_unfinished_jobs(&store, &mut next)
                .unwrap(),
            1
        );
        let told = queued_failures(&store, &[1; 32]);
        assert_eq!(told.len(), 1);
        // The record as the crash left it: told, not saved.
        store.save_compute_job(&before).unwrap();

        assert_eq!(
            ComputeTransportState::default()
                .fail_unfinished_jobs(&store, &mut next)
                .unwrap(),
            1
        );
        assert_eq!(queued_failures(&store, &[1; 32]), told);
        let record = store.load_compute_job(&[9; 16]).unwrap().unwrap();
        assert_eq!(
            (record.lifecycle.phase, record.lifecycle.sequence),
            (told[0].2.phase.clone(), told[0].2.sequence)
        );
        assert_eq!(
            record.log_bytes,
            u64::try_from(told[0].2.log_tail.len()).unwrap()
        );
    }

    /// A session ending after a newer session of the same device attached
    /// leaves the newer one's state alone; the current session releases it.
    #[test]
    fn only_the_current_session_releases_a_devices_compute_state() {
        let mut state = ComputeTransportState::default();
        state
            .advertise([2; 32], 7, 200, advertisement(false))
            .unwrap();
        let (worker, _deliveries) = mpsc::channel(4);
        let (old_session, _old) = mpsc::channel(1);
        let (new_session, _new) = mpsc::channel(1);
        state.attach([2; 32], worker, new_session.clone(), None);
        assert!(!state.detach_session(&[2; 32], &old_session));
        assert!(
            state.workers.contains_key(&[2; 32]),
            "the newer session keeps it"
        );
        assert!(state.detach_session(&[2; 32], &new_session));
        assert!(!state.workers.contains_key(&[2; 32]));
        assert!(state.worker_connection(&[2; 32]).is_none());
    }

    /// The worker's advertisement samples the RTT its live compute connection
    /// already measures; doing so spawns nothing, so an idle relay gains no
    /// task or timer to wake for.
    #[tokio::test(flavor = "current_thread")]
    async fn advertisement_samples_the_live_connection_rtt_without_new_tasks() {
        use iroh::endpoint::presets;
        const ALPN: &[u8] = b"tilo/test/compute-latency";

        let relay = iroh::Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .unwrap();
        let worker = iroh::Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let (dialled, accepted) = tokio::join!(worker.connect(relay.addr(), ALPN), async {
            relay.accept().await.unwrap().await.unwrap()
        });
        let _dialled = dialled.unwrap();

        let now_ms = tilo_clock::unix_now_ms();
        let mut live = advertisement(false);
        live.issued_at_ms = now_ms;
        live.expires_at_ms = now_ms.saturating_add(MAX_COMPUTE_ADVERTISEMENT_LIFETIME_MS);
        let mut state = ComputeTransportState::default();
        let runtime = tokio::runtime::Handle::current().metrics();
        let tasks_before = runtime.num_alive_tasks();

        let (sender, _receiver) = mpsc::channel(1);
        let (disconnect, _disconnected) = mpsc::channel(1);
        state.attach([2; 32], sender, disconnect, Some(accepted));
        assert_eq!(state.latency_ms(&[2; 32]), UNMEASURED_LATENCY_MS);
        state.advertise([2; 32], 7, now_ms, live).unwrap();
        let placement = state.place(&spec().requirements(), now_ms).unwrap();

        assert_eq!(placement.worker, [2; 32]);
        assert!(
            (1..UNMEASURED_LATENCY_MS).contains(&placement.rank.latency_ms),
            "measured latency is reported: {placement}"
        );
        assert_eq!(placement.rank.latency_ms, state.latency_ms(&[2; 32]));
        assert_eq!(
            runtime.num_alive_tasks(),
            tasks_before,
            "measuring latency must not spawn a probe"
        );
        worker.close().await;
        relay.close().await;
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn compute_transport_binds_requests_routes_and_results() {
        // Phase `request_binding_survives_restart_and_replay_is_idempotent`.
        {
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
        // Phase `forged_route_and_stale_or_ineligible_worker_are_rejected`.
        {
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
        // Phase `cancellation_is_durable_idempotent_and_route_bound`.
        {
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
        // Phase `status_result_are_monotonic_bounded_and_worker_bound`.
        {
            let root = tempfile::tempdir().unwrap();
            let (mut state, store) = admitted(root.path());
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

            let mut oversized = status;
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
        // Phase `partitioned_route_queues_on_dedicated_protocol_and_live_route_does_not`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = open_store(root.path());
            let mut state = ComputeTransportState::default();
            let envelope = ComputeEnvelope::Cancel { binding: binding() };
            state.route([2; 32], &envelope, &store, 1).unwrap();
            let pending = store.load_compute_pending(&[2; 32]).unwrap();
            assert_eq!(pending.len(), 1);
            assert!(matches!(
                postcard::from_bytes::<ComputeEnvelope>(&pending[0].bytes).unwrap(),
                ComputeEnvelope::Delivery { sequence: 1, message, .. } if *message == envelope
            ));

            let (sender, mut receiver) = mpsc::channel(1);
            let (disconnect, _disconnect_receiver) = mpsc::channel(1);
            state.attach([2; 32], sender, disconnect, None);
            let live = ComputeEnvelope::Cancel { binding: binding() };
            state.route([2; 32], &live, &store, 2).unwrap();
            assert!(matches!(
                receiver.try_recv().unwrap(),
                ComputeEnvelope::Delivery { sequence: 2, message, .. } if *message == live
            ));
            assert_eq!(store.load_compute_pending(&[2; 32]).unwrap().len(), 2);
        }
    }
}
