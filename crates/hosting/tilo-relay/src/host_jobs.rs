//! The transforms a host runs for its workspaces' devices: an offloadable
//! action's declared entrypoint, run as a compute job over the content the
//! host keeps.
//!
//! This is the host's own side of the offer round
//! ([`tilo_room_host::workspaces::HostJobs`]). A device of a workspace the host
//! governs offers it a transform; the host answers with its standing and the
//! inputs it already keeps, and the device that decides sends the invocation
//! to whichever device wins. When that is this host, the invocation runs here
//! as a signed package's entrypoint under the launcher sandbox, reading only
//! the inputs the offer named, all of which are content the host already
//! keeps: no bytes move to run a job it holds the inputs for.
//!
//! What it produces is kept in the same content store, so the device that
//! asked fetches each output by the `tilo.content` address the answer names,
//! as it reads any other content of the workspace.
//!
//! It grants nothing: the declaration bounds the work, the package is verified
//! against its signed root before it runs, and the job is admitted by the same
//! rules as any compute job.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use launcher::capture::CaptureCancellation;
use neighbour_wire::offer::{MAX_WORK_INPUTS, Work};
use tilo_actions::remote::{ActionHostError, InvokeRequest, Pending, pending};
use tilo_actions::{ActionOutput, InvocationId, Offload};
use tilo_clock::unix_now_ms;
use tilo_room_host::workspaces::HostJobs;

use crate::compute::{
    COMPUTE_JOB_SCHEMA_VERSION, ComputeContentStore, ComputeJobPhase, ComputeJobSpec,
    ComputePackageCatalogue, ComputeResourceLimits, ComputeSandbox, ComputeWorker,
    MAX_COMPUTE_JOB_LIFETIME_MS,
};
use crate::mesh::{DeviceRole, PowerAvailability, ServerAdvertisement, ServiceCapabilities};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;

/// The host's transforms: the packages it can run, the sandbox it runs them
/// under, and the content it reads and keeps.
#[derive(Clone)]
pub struct ComputeJobs {
    shared: Arc<Shared>,
}

struct Shared {
    /// The content this host keeps for its workspaces: the inputs a job reads,
    /// and where its outputs are kept for the device that asked.
    content: Arc<dyn ComputeContentStore>,
    catalogue: Arc<dyn ComputePackageCatalogue>,
    sandbox: Arc<dyn ComputeSandbox>,
    /// The host's own device: the worker of every job it runs.
    device: [u8; 32],
    data_home: PathBuf,
    state_root: PathBuf,
    /// How many jobs run here at once.
    capacity: AtomicU32,
    /// The jobs running here, so the device that asked stops them: its
    /// length is what is in hand, held with the admission that adds to it.
    running: std::sync::Mutex<std::collections::HashMap<InvocationId, CaptureCancellation>>,
    runtime: tokio::runtime::Handle,
}

impl ComputeJobs {
    /// Run transforms for `device`, reading and keeping content in `content`,
    /// with its packages resolved by `catalogue` and launched by `sandbox`.
    /// Jobs stage their work under `state_root` and their app directories
    /// under `data_home`.
    ///
    /// # Panics
    ///
    /// Outside a runtime context, where there is nothing to run on.
    #[must_use]
    pub fn new(
        content: Arc<dyn ComputeContentStore>,
        catalogue: Arc<dyn ComputePackageCatalogue>,
        sandbox: Arc<dyn ComputeSandbox>,
        device: [u8; 32],
        data_home: PathBuf,
        state_root: PathBuf,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                content,
                catalogue,
                sandbox,
                device,
                data_home,
                state_root,
                capacity: AtomicU32::new(
                    u32::try_from(
                        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
                    )
                    .unwrap_or(1),
                ),
                running: std::sync::Mutex::default(),
                runtime: tokio::runtime::Handle::current(),
            }),
        }
    }

    /// Take on at most `capacity` jobs at once, rather than one per processor:
    /// for a host that keeps part of the machine for its own work.
    #[must_use]
    pub fn with_capacity(self, capacity: u32) -> Self {
        self.shared.capacity.store(capacity, Ordering::Relaxed);
        self
    }

    /// How many more jobs this host can start now: what an offer is answered
    /// with, so a host at its limit declines rather than wins a round it
    /// would refuse at run.
    fn free_slots(&self) -> u32 {
        self.shared
            .capacity
            .load(Ordering::Relaxed)
            .saturating_sub(self.in_flight())
    }

    /// How many jobs run here now.
    fn in_flight(&self) -> u32 {
        u32::try_from(self.shared.running.lock_recover().len()).unwrap_or(u32::MAX)
    }

    /// Whether `offload` names work this host could run: a declaration that
    /// could be placed as a compute job, whose package is installed here.
    fn can_run(&self, offload: &Offload) -> bool {
        offload.validate().is_empty()
            && self
                .shared
                .catalogue
                .resolve(&offload.app)
                .is_ok_and(|installed| installed.is_some())
    }

    /// What worker this host is for a job of `app` in `workspace`.
    fn advertisement(&self, workspace: WorkspaceId, app: &str) -> ServerAdvertisement {
        ServerAdvertisement {
            device: self.shared.device,
            role: DeviceRole::AlwaysOnServer,
            services: ServiceCapabilities {
                compute: true,
                ..ServiceCapabilities::default()
            },
            workspaces: std::collections::BTreeSet::from([workspace]),
            apps: std::collections::BTreeSet::from([app.to_owned()]),
            accelerators: std::collections::BTreeSet::new(),
            priority: 0,
            latency_ms: 0,
            power: PowerAvailability::External,
            energy_saver: false,
            active_assignments: self.in_flight(),
            assignment_capacity: self.shared.capacity.load(Ordering::Relaxed),
            online: true,
        }
    }

    /// What `work` names, when it is work this host takes in `workspace`.
    fn taken(&self, workspace: &str, work: &Work) -> Option<Job> {
        let offload = work.offload.clone()?;
        if !self.can_run(&offload) {
            return None;
        }
        let workspace_id = WorkspaceId::parse(workspace)?;
        let inputs: Vec<[u8; 32]> = work.inputs.iter().map(|(digest, _)| *digest).collect();
        // Inputs travel as their digest each up to the wire's bound, and as
        // their total past it: a job whose inputs were not all named is not
        // one this host could run on what the offer described.
        let named = work
            .inputs
            .iter()
            .fold(0_u64, |sum, (_, bytes)| sum.saturating_add(*bytes));
        if inputs.len() >= MAX_WORK_INPUTS && named < work.input_bytes {
            return None;
        }
        Some(Job {
            workspace_id,
            offload,
            inputs,
        })
    }
}

/// One job the host was offered: what runs, where, and over what.
struct Job {
    workspace_id: WorkspaceId,
    offload: Offload,
    inputs: Vec<[u8; 32]>,
}

impl HostJobs for ComputeJobs {
    fn runs(&self, workspace: &str, work: &Work) -> bool {
        self.taken(workspace, work).is_some()
    }

    fn free_slots(&self) -> Option<u32> {
        Some(self.free_slots())
    }

    fn holds(&self, _workspace: &str, work: &Work) -> Vec<[u8; 32]> {
        work.inputs
            .iter()
            .map(|(digest, _)| *digest)
            .filter(|digest| {
                self.shared
                    .content
                    .get(*digest)
                    .is_ok_and(|bytes| bytes.is_some())
            })
            .collect()
    }

    fn run(
        &self,
        workspace: &str,
        work: &Work,
        requester: [u8; 32],
        request: InvokeRequest,
    ) -> Pending<Result<ActionOutput, ActionHostError>> {
        let Some(job) = self.taken(workspace, work) else {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        };
        let package = self
            .shared
            .catalogue
            .resolve(&job.offload.app)
            .ok()
            .flatten()
            .and_then(|installed| {
                let mut digest = [0; 32];
                hex::decode_to_slice(&installed.package_id_hex, &mut digest)
                    .ok()
                    .map(|()| digest)
            });
        let Some(package_digest) = package else {
            return Pending::ready(Err(ActionHostError::Unavailable {
                reason: "the transform's package is not installed here".into(),
            }));
        };
        let spec = match job_spec(
            requester,
            self.shared.device,
            (job.workspace_id, package_digest),
            &job.offload,
            &job.inputs,
        ) {
            Ok(spec) => spec,
            Err(reason) => return Pending::ready(Err(ActionHostError::Refused { reason })),
        };
        let (workspace_id, app) = (job.workspace_id, job.offload.app);
        let invocation = request.invocation;
        let cancellation = CaptureCancellation::default();
        // Admission is one decision with the count it reads: a job is taken
        // only while this host has a slot for it, never a round after it
        // said so.
        {
            let mut running = self.shared.running.lock_recover();
            let in_flight = u32::try_from(running.len()).unwrap_or(u32::MAX);
            if in_flight >= self.shared.capacity.load(Ordering::Relaxed) {
                return Pending::ready(Err(ActionHostError::Unavailable {
                    reason: "this host is running as many jobs as it takes on".into(),
                }));
            }
            running.insert(invocation, cancellation.clone());
        }
        let jobs = self.clone();
        let (complete, answer) = pending();
        self.shared.runtime.spawn(async move {
            let ran = jobs
                .execute(workspace_id, &app, spec, requester, cancellation)
                .await;
            jobs.shared.running.lock_recover().remove(&invocation);
            complete.complete(ran);
        });
        answer
    }

    fn cancel(&self, invocation: InvocationId) {
        if let Some(cancellation) = self.shared.running.lock_recover().get(&invocation) {
            cancellation.cancel();
        }
    }
}

impl ComputeJobs {
    /// Run `spec` here and keep what it produced, as the action output that
    /// names each output.
    async fn execute(
        &self,
        workspace_id: WorkspaceId,
        app: &str,
        spec: ComputeJobSpec,
        requester: [u8; 32],
        cancellation: CaptureCancellation,
    ) -> Result<ActionOutput, ActionHostError> {
        let failed = |reason: String| ActionHostError::Refused { reason };
        let shared = Arc::clone(&self.shared);
        let advertisement = self.advertisement(workspace_id, app);
        let executed = tokio::task::spawn_blocking(move || {
            let worker = ComputeWorker::new(
                advertisement,
                shared.data_home.clone(),
                shared.state_root.clone(),
                Arc::clone(&shared.content),
                Arc::clone(&shared.catalogue),
                Arc::clone(&shared.sandbox),
            );
            worker.execute(requester, workspace_id, &spec, unix_now_ms(), cancellation)
        })
        .await;
        let result = match executed {
            Ok(result) => {
                result.map_err(|error| failed(format!("the job did not run: {error}")))?
            }
            Err(error) => return Err(failed(format!("the job did not finish: {error}"))),
        };
        match result.lifecycle.phase {
            ComputeJobPhase::Completed { outputs } => Ok(content_output(&outputs)),
            ComputeJobPhase::Cancelled => Err(failed("the job was stopped here".into())),
            ComputeJobPhase::Failed { code } => {
                Err(failed(format!("the transform failed here: {code}")))
            }
            phase => Err(failed(format!("the job did not complete: {phase:?}"))),
        }
    }
}

/// The action output of `outputs`: each a content resource, in order.
fn content_output(outputs: &[[u8; 32]]) -> ActionOutput {
    ActionOutput {
        value: None,
        resources: outputs
            .iter()
            .filter_map(|digest| tilo_actions::content_resource(&hex::encode(digest)))
            .collect(),
    }
}

/// A job binding `offload` over `inputs` from `requester` to `worker`.
fn job_spec(
    requester: [u8; 32],
    worker: [u8; 32],
    (workspace_id, package_digest): (WorkspaceId, [u8; 32]),
    offload: &Offload,
    inputs: &[[u8; 32]],
) -> Result<ComputeJobSpec, String> {
    let mut job_id = [0; 16];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut job_id);
    let issued_at_ms = unix_now_ms();
    // Workspace to read the inputs and run the declared CPU time, never past a
    // job's ceiling.
    let lifetime = offload
        .cpu_time_ms
        .saturating_mul(2)
        .saturating_add(30_000)
        .min(MAX_COMPUTE_JOB_LIFETIME_MS);
    let spec = ComputeJobSpec {
        version: COMPUTE_JOB_SCHEMA_VERSION,
        job_id,
        requester,
        worker,
        workspace_id,
        app_id: offload.app.clone(),
        package_digest,
        entrypoint: offload.entrypoint.clone(),
        arguments: Vec::new(),
        inputs: inputs.to_vec(),
        limits: ComputeResourceLimits {
            cpu_time_ms: offload.cpu_time_ms,
            memory_bytes: offload.memory_bytes,
            output_bytes: offload.output_bytes,
            gpu: offload.gpu,
        },
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(lifetime),
    };
    spec.validate_for(worker, issued_at_ms)
        .map_err(|error| format!("the job cannot be bound: {error}"))?;
    Ok(spec)
}
