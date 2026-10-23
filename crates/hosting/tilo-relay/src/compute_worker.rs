//! Production owner for the signed-package compute executor.
//!
//! The owner advertises only after verifying the installed catalogue,
//! launcher, private content store, and local power policy. Its advertisement
//! lives on the dedicated authenticated session, so task failure, health
//! regression, shutdown, or connection loss withdraws eligibility without a
//! stale durable capability.
//!
//! A routed job's inputs are hydrated from its requester through the relay
//! into a private per-job directory ([`hydrate_job_inputs`]) — or, for an
//! input this worker already holds and the requester proves it holds too,
//! from the worker's own copy, with no bytes moving — and its outputs are
//! retained for that requester to fetch by digest ([`ComputeOutputSource`])
//! for [`OUTPUT_RETENTION`]. Retained outputs are themselves held content:
//! a job chained on an earlier job's output runs without the requester
//! sending it back.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use launcher::capture::CaptureCancellation;
use launcher::manifest::read_manifest;
use thiserror::Error;
use tilo_clock::unix_now_ms;
use tilo_user_identity::{DeviceCert, RevocationList};

use crate::compute::{
    ComputeContentStore, ComputeExecutionError, ComputeJobPhase, ComputeJobResult, ComputeJobSpec,
    ComputeOutputSource, ComputePackageCatalogue, ComputeSandbox, ComputeWorker, HeldContent,
    InstalledComputeCatalogue, LauncherComputeSandbox, MAX_COMPUTE_INPUT_BYTES,
    MAX_COMPUTE_JOB_LIFETIME_MS, OUTPUT_INDEX_SUFFIX,
};
use crate::compute_content::{
    ContentFetchError, answer_content, content_proof, fetch_job_content, prove_job_input,
};
use crate::compute_session::{ComputeClient, ComputeDelivery, ComputeSessionError};
use crate::compute_transport::{
    COMPUTE_TRANSPORT_SCHEMA_VERSION, ComputeEnvelope, ComputeJobBinding, ComputeResultUpdate,
    ComputeStatusUpdate, ComputeWorkerAdvertisement, MAX_COMPUTE_LOG_BYTES,
};
use crate::mesh::{
    ComputeAccelerator, DeviceRole, PowerAvailability, ServerAdvertisement, ServiceCapabilities,
};
use crate::offload_content::ReplicaContent;

const HEALTH_INTERVAL: Duration = Duration::from_secs(10);
const ADVERTISEMENT_LIFETIME_MS: u64 = 30_000;
const DEFAULT_ASSIGNMENT_CAPACITY: u32 = 1;
/// How long a completed job's outputs stay retrievable by its requester.
/// The result reaches an offline requester whenever it reattaches, so this
/// spans a day rather than the job's lifetime.
pub const OUTPUT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const OUTPUT_PRUNE_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// How old a job's scratch must be before it is taken for a killed job's:
/// past any job's lifetime, so no live job, of this worker or of another
/// sharing the directory, can still be using it.
pub const STALE_JOB_SCRATCH: Duration =
    Duration::from_millis(MAX_COMPUTE_JOB_LIFETIME_MS.saturating_mul(2));

/// Failure while establishing or owning the production worker.
#[derive(Debug, Error)]
pub enum ComputeWorkerError {
    /// Catalogue, launcher, policy, or power health is not eligible.
    #[error("compute worker health check failed: {0}")]
    Health(String),
    /// The private content-addressed staging store failed.
    #[error("compute worker content store: {0}")]
    Content(String),
    /// The dedicated authenticated compute session failed.
    #[error(transparent)]
    Session(#[from] ComputeSessionError),
}

/// Private content-addressed bytes used by the worker staging boundary.
#[derive(Clone, Debug)]
pub struct FileComputeContentStore {
    root: PathBuf,
}

impl FileComputeContentStore {
    /// Open and probe a private content-addressed store.
    ///
    /// # Errors
    ///
    /// Returns an error if the root cannot be created with private
    /// permissions, or a write/read/remove health probe fails.
    pub fn open(root: PathBuf) -> Result<Self, ComputeWorkerError> {
        fs::create_dir_all(&root)
            .map_err(|error| ComputeWorkerError::Content(error.to_string()))?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .map_err(|error| ComputeWorkerError::Content(error.to_string()))?;
        let store = Self { root };
        store.probe()?;
        Ok(store)
    }

    fn probe(&self) -> Result<(), ComputeWorkerError> {
        let path = self.root.join(format!(".health-{}", std::process::id()));
        let result = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(b"compute-health")?;
            file.sync_all()?;
            let mut bytes = Vec::new();
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&path)?
                .read_to_end(&mut bytes)?;
            if bytes != b"compute-health" {
                return Err(std::io::Error::other("content-store probe mismatch"));
            }
            fs::remove_file(&path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(path);
        }
        result.map_err(|error| ComputeWorkerError::Content(error.to_string()))
    }

    fn path_for(&self, digest: [u8; 32]) -> PathBuf {
        self.root.join(hex::encode(digest))
    }
}

impl ComputeContentStore for FileComputeContentStore {
    fn get(&self, digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
        let path = self.path_for(digest);
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_COMPUTE_INPUT_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_COMPUTE_INPUT_BYTES
            || blake3::hash(&bytes).as_bytes() != &digest
        {
            return Err("content bytes do not match their bounded digest".into());
        }
        Ok(Some(bytes))
    }

    fn put(&self, digest: [u8; 32], bytes: &[u8]) -> Result<(), String> {
        if blake3::hash(bytes).as_bytes() != &digest {
            return Err("refusing content under a mismatched digest".into());
        }
        let path = self.path_for(digest);
        if path.exists() {
            return self.get(digest).map(|_| ());
        }
        tilo_files::native::atomic::write(&path, bytes).map_err(|error| error.to_string())
    }
}

impl HeldContent for FileComputeContentStore {
    fn held(
        &self,
        digest: [u8; 32],
        _locator: Option<[u8; 32]>,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        let len = match fs::symlink_metadata(self.path_for(digest)) {
            Ok(metadata) if metadata.is_file() => metadata.len(),
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        if len > max_bytes {
            return Ok(None);
        }
        self.get(digest)
    }
}

/// Content a worker holds in several places: the first source that holds
/// an input serves it, and one that cannot be read does not stop the rest.
struct HeldSources(Vec<Arc<dyn HeldContent>>);

impl HeldContent for HeldSources {
    fn held(
        &self,
        digest: [u8; 32],
        locator: Option<[u8; 32]>,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, String> {
        let mut failure = None;
        for source in &self.0 {
            match source.held(digest, locator, max_bytes) {
                Ok(Some(bytes)) => return Ok(Some(bytes)),
                Ok(None) => {}
                Err(error) => failure = Some(error),
            }
        }
        failure.map_or(Ok(None), Err)
    }

    fn locates(&self) -> bool {
        self.0.iter().any(|source| source.locates())
    }
}

/// What a production worker holds for the jobs routed to it: the outputs
/// it retains, for a job in any workspace, and the filesystem replica this
/// device keeps of a workspace ([`ReplicaContent`]), for that workspace's
/// jobs only. Either is used only once the job's requester proves it holds
/// the same bytes ([`hydrate_job_inputs`]).
#[derive(Clone)]
pub struct WorkerHeldContent {
    outputs: Arc<dyn HeldContent>,
    by_workspace: HashMap<u64, Arc<dyn HeldContent>>,
}

impl WorkerHeldContent {
    /// `outputs`, plus each workspace's replica stores in `replicas`.
    #[must_use]
    pub fn new(outputs: Arc<dyn HeldContent>, replicas: &BTreeMap<u64, Vec<PathBuf>>) -> Self {
        let by_workspace = replicas
            .iter()
            .filter(|(_, dirs)| !dirs.is_empty())
            .map(|(&workspace_id, dirs)| {
                let replica: Arc<dyn HeldContent> = Arc::new(ReplicaContent::new(dirs.clone()));
                let sources: Arc<dyn HeldContent> =
                    Arc::new(HeldSources(vec![Arc::clone(&outputs), replica]));
                (workspace_id, sources)
            })
            .collect();
        Self {
            outputs,
            by_workspace,
        }
    }

    /// What a job in `workspace_id` may run on in place of fetching.
    #[must_use]
    pub fn for_workspace(&self, workspace_id: u64) -> Arc<dyn HeldContent> {
        self.by_workspace
            .get(&workspace_id)
            .map_or_else(|| Arc::clone(&self.outputs), Arc::clone)
    }
}

/// Inputs needed to own a production worker session.
pub struct ProductionComputeWorkerConfig {
    /// Local endpoint used to originate the authenticated worker session.
    pub endpoint: iroh::endpoint::Endpoint,
    /// Exact relay endpoint to which the worker attaches.
    pub relay_address: iroh_base::EndpointAddr,
    /// User root that must certify both ends of the session.
    pub user_public_key: [u8; 32],
    /// Worker device certificate.
    pub device_cert: DeviceCert,
    /// Worker key used for the mutual-authentication proof.
    pub device_signing_key: ed25519_dalek::SigningKey,
    /// Current device revocation view.
    pub revocations: RevocationList,
    /// Exact configured workspaces this owner may advertise into.
    pub workspaces: BTreeSet<u64>,
    /// Private worker state, staging, and replay root.
    pub state_root: PathBuf,
    /// Process-local readiness projection cleared on every exit path.
    pub readiness: Arc<AtomicBool>,
    /// Environment sessions and legacy apps this worker hosts for members.
    pub environments: WorkerEnvironments,
    /// The filesystem replica stores this device keeps, by relay workspace
    /// (a home peer's cache directories): content a job in that workspace
    /// may run on once its requester proves it holds the same bytes.
    pub replicas: BTreeMap<u64, Vec<PathBuf>>,
}

/// What a worker hosts for members besides batch jobs.
#[derive(Clone, Default)]
pub struct WorkerEnvironments {
    /// Interactive environment sessions.
    pub host: Option<crate::environment_host::EnvironmentHost>,
    /// Legacy apps, on a worker with KVM.
    pub legacy: Option<crate::legacy_host::LegacyWorker>,
    /// Where the hosted environments' legacy launches go: the relay, over
    /// this worker's connection once it is attached.
    #[cfg(target_os = "linux")]
    pub legacy_route: Option<crate::legacy_route::LegacyLaunchRoute>,
}

impl WorkerEnvironments {
    fn capacity(&self) -> (u32, u32) {
        self.host.as_ref().map_or((0, 0), |host| {
            (
                host.config().capacity,
                u32::try_from(host.active_sessions()).unwrap_or(u32::MAX),
            )
        })
    }

    fn accelerators(&self) -> BTreeSet<ComputeAccelerator> {
        if self.legacy.is_some() && Path::new("/dev/kvm").exists() {
            BTreeSet::from([ComputeAccelerator::Kvm])
        } else {
            BTreeSet::new()
        }
    }

    fn hosts_anything(&self) -> bool {
        self.host.is_some() || self.legacy.is_some()
    }
}

/// Serve a stream the relay opened to this worker on a task of its own;
/// `outputs` answers for the outputs of the jobs this worker ran.
pub fn spawn_worker_stream(
    environments: WorkerEnvironments,
    outputs: Option<Arc<dyn ComputeOutputSource>>,
    (mut send, mut receive): (iroh::endpoint::SendStream, iroh::endpoint::RecvStream),
) {
    tokio::spawn(async move {
        if let Err(error) =
            serve_worker_stream_with(environments, outputs, &mut send, &mut receive).await
        {
            tracing::debug!(%error, "worker environment stream ended");
        }
        let _ = send.finish();
    });
}

/// Serve one stream the relay opened to this worker, which runs no batch
/// jobs: it has no job outputs to serve.
pub async fn serve_worker_stream<S, R>(
    environments: WorkerEnvironments,
    send: &mut S,
    receive: &mut R,
) -> Result<(), crate::environment_host::EnvironmentStreamError>
where
    S: tokio::io::AsyncWrite + Unpin + Send,
    R: tokio::io::AsyncRead + Unpin + Send,
{
    serve_worker_stream_with(environments, None, send, receive).await
}

/// Serve one stream the relay opened to this worker: an environment
/// session, a legacy app, or an output of a job it ran, from `outputs`
/// ([`WorkerStreamOpen::ComputeContent`](tilo_relay_protocol::environment::WorkerStreamOpen::ComputeContent)).
pub async fn serve_worker_stream_with<S, R>(
    environments: WorkerEnvironments,
    outputs: Option<Arc<dyn ComputeOutputSource>>,
    send: &mut S,
    receive: &mut R,
) -> Result<(), crate::environment_host::EnvironmentStreamError>
where
    S: tokio::io::AsyncWrite + Unpin + Send,
    R: tokio::io::AsyncRead + Unpin + Send,
{
    use crate::environment_host::{read_message, write_message};
    use tilo_relay_protocol::environment::{
        ContentReply, ENVIRONMENT_PROTOCOL_VERSION, LegacyFrame, Refusal, ServerFrame,
        WorkerStreamOpen,
    };
    let Some(open) = read_message::<_, WorkerStreamOpen>(receive).await? else {
        return Ok(());
    };
    match open {
        // Proofs are a requester's to give, for the inputs of jobs it
        // submitted; a worker proves nothing of what it retains.
        WorkerStreamOpen::ComputeContentProof { .. } => {
            crate::compute_content::refuse_proof(send, "not_authorised").await
        }
        WorkerStreamOpen::ComputeContent {
            version,
            workspace_id,
            asker,
            job_id,
            digest,
        } => {
            let code = match outputs {
                Some(outputs) if version == ENVIRONMENT_PROTOCOL_VERSION => {
                    let found = tokio::task::spawn_blocking(move || {
                        outputs.retained_output(asker.device, workspace_id, job_id, digest)
                    })
                    .await
                    .unwrap_or_else(|_| Err("retained output read panicked".into()));
                    return answer_content(send, digest, found).await;
                }
                Some(_) => "unsupported_version",
                None => "content_plane_unavailable",
            };
            write_message(send, &ContentReply::Refused { code: code.into() }).await
        }
        WorkerStreamOpen::Environment {
            version,
            member,
            request,
            session,
            ..
        } => match environments.host {
            Some(host) if version == ENVIRONMENT_PROTOCOL_VERSION => {
                host.serve(member, session, request, send, receive).await
            }
            _ => {
                write_message(
                    send,
                    &ServerFrame::Refused {
                        reason: Refusal::NoWorker,
                        detail: "this worker runs no environments".into(),
                    },
                )
                .await
            }
        },
        WorkerStreamOpen::Legacy {
            version,
            member,
            launch,
            request,
            ..
        } => match environments.legacy {
            Some(legacy) if version == ENVIRONMENT_PROTOCOL_VERSION => {
                legacy.serve(member, launch, request, send, receive).await
            }
            _ => {
                write_message(
                    send,
                    &LegacyFrame::Refused {
                        reason: Refusal::NoWorker,
                        detail: "this worker runs no legacy apps".into(),
                    },
                )
                .await
            }
        },
    }
}

struct WorkerHealth {
    apps: BTreeSet<String>,
    power: PowerAvailability,
}

/// Run the production worker until shutdown, health regression, or transport
/// failure. Dropping/aborting this future closes the session and withdraws
/// every advertisement.
pub async fn run_production_compute_worker(
    config: ProductionComputeWorkerConfig,
) -> Result<(), ComputeWorkerError> {
    let health = inspect_health(config.environments.hosts_anything())?;
    if config.workspaces.is_empty() {
        return Err(ComputeWorkerError::Health(
            "worker has no configured workspace".into(),
        ));
    }
    let data_home = launcher::appdir::host_data_home()
        .ok_or_else(|| ComputeWorkerError::Health("launcher has no trusted data home".into()))?;
    let content = FileComputeContentStore::open(config.state_root.join("content"))?;
    // The outputs this worker retains are content it holds: a job chained
    // on one runs without its requester sending it back. So is a workspace
    // replica this device keeps: a job on its files runs on them in place.
    let held = WorkerHeldContent::new(Arc::new(content.clone()), &config.replicas);
    let advertisement = server_advertisement(&config, &health, 0);
    let executor = Arc::new(ComputeWorker::new(
        advertisement,
        data_home,
        config.state_root.join("execution"),
        content,
        InstalledComputeCatalogue,
        LauncherComputeSandbox::new(None),
    ));
    let mut client = ComputeClient::connect(
        config.endpoint.clone(),
        config.relay_address.clone(),
        config.user_public_key,
        Some(config.device_cert.device_public_key),
        config.device_cert.clone(),
        config.device_signing_key.clone(),
        config.revocations.clone(),
    )
    .await?;
    attach_workspaces(&mut client, &config.workspaces).await?;

    advertise_all(&mut client, &config, &health, 0).await?;
    config.readiness.store(true, Ordering::Release);
    let _withdraw_on_drop = ReadinessGuard(Arc::clone(&config.readiness));
    let assignment_capacity = usize::try_from(DEFAULT_ASSIGNMENT_CAPACITY).map_err(|_| {
        ComputeWorkerError::Health("assignment capacity is not representable".into())
    })?;
    let (completed_tx, mut completed_rx) = tokio::sync::mpsc::channel(assignment_capacity);
    let mut active = ActiveJobs::default();
    let mut health_tick = tokio::time::interval(HEALTH_INTERVAL);
    health_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    health_tick.tick().await;
    let mut prune_tick = tokio::time::interval(OUTPUT_PRUNE_INTERVAL);
    prune_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let _reaper = config
        .environments
        .host
        .as_ref()
        .map(|host| AbortOnDrop(host.spawn_reaper()));
    let connection = client.connection();
    // Hosted environments send their legacy launches up this connection.
    #[cfg(target_os = "linux")]
    if let Some(route) = &config.environments.legacy_route {
        route.set_relay(connection.clone());
    }
    let outputs: Arc<dyn ComputeOutputSource> = executor.clone();

    loop {
        tokio::select! {
            stream = connection.accept_bi() => {
                let stream = stream
                    .map_err(|error| ComputeSessionError::Connection(error.to_string()))?;
                spawn_worker_stream(config.environments.clone(), Some(Arc::clone(&outputs)), stream);
            }
            _ = prune_tick.tick() => spawn_output_prune(&config.state_root),
            delivery = client.receive() => {
                let delivery = delivery?;
                handle_delivery(
                    &mut client,
                    &config,
                    &health,
                    (&executor, &held),
                    &completed_tx,
                    &mut active,
                    delivery,
                ).await?;
            }
            completed = completed_rx.recv() => {
                let Some(completed) = completed else {
                    return Err(ComputeWorkerError::Health("compute completion channel closed".into()));
                };
                active.remove(&completed.binding.job_id);
                send_completion(&mut client, completed).await?;
                advertise_all(&mut client, &config, &health, active.len()).await?;
            }
            _ = health_tick.tick() => {
                let refreshed = inspect_health(config.environments.hosts_anything())?;
                if !health_unchanged(&health, &refreshed) {
                    return Err(ComputeWorkerError::Health(
                        "worker catalogue or power health changed; withdrawing before re-advertising".into(),
                    ));
                }
                advertise_all(&mut client, &config, &health, active.len()).await?;
            }
        }
    }
}

/// Attach the worker session to each configured workspace, failing when the
/// relay refuses any of them.
async fn attach_workspaces(
    client: &mut ComputeClient,
    workspaces: &BTreeSet<u64>,
) -> Result<(), ComputeWorkerError> {
    for workspace in workspaces {
        client.attach(*workspace).await?;
        let delivery = client.receive().await?;
        if !matches!(
            delivery.envelope,
            ComputeEnvelope::Ready { workspace_id, .. } if workspace_id == *workspace
        ) {
            return Err(ComputeWorkerError::Health(
                "relay rejected compute worker workspace attachment".into(),
            ));
        }
    }
    Ok(())
}

/// Prune retained outputs, and the scratch of jobs a killed worker left,
/// under `state_root` off the async runtime. The first prune runs as the
/// worker starts.
fn spawn_output_prune(state_root: &Path) {
    let root = state_root.to_path_buf();
    let execution = state_root.join("execution");
    let content = state_root.join("content");
    tokio::task::spawn_blocking(move || {
        match prune_stale_job_scratch(&root, std::time::SystemTime::now(), STALE_JOB_SCRATCH) {
            Ok(0) => {}
            Ok(removed) => tracing::info!(removed, "removed the scratch of killed compute jobs"),
            Err(error) => tracing::warn!(%error, "pruning killed compute jobs' scratch failed"),
        }
        if let Err(error) = prune_retained_outputs(
            &execution,
            &content,
            std::time::SystemTime::now(),
            OUTPUT_RETENTION,
        ) {
            tracing::warn!(%error, "pruning retained compute outputs failed");
        }
    });
}

struct ActiveJob {
    binding: ComputeJobBinding,
    cancellation: CaptureCancellation,
}

#[derive(Default)]
struct ActiveJobs {
    by_job_id: HashMap<[u8; 16], ActiveJob>,
}

impl ActiveJobs {
    fn get(&self, job_id: &[u8; 16]) -> Option<&ActiveJob> {
        self.by_job_id.get(job_id)
    }

    fn insert(&mut self, job_id: [u8; 16], job: ActiveJob) {
        self.by_job_id.insert(job_id, job);
    }

    fn remove(&mut self, job_id: &[u8; 16]) -> Option<ActiveJob> {
        self.by_job_id.remove(job_id)
    }

    fn len(&self) -> usize {
        self.by_job_id.len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.by_job_id.is_empty()
    }

    /// Revoke every in-process execution before forgetting its exact route.
    /// `drain` makes shutdown cleanup deterministic even when a worker future
    /// is aborted while its blocking launcher task is still unwinding.
    fn cancel_all(&mut self) -> usize {
        let count = self.by_job_id.len();
        for (_, job) in self.by_job_id.drain() {
            job.cancellation.cancel();
        }
        count
    }
}

impl Drop for ActiveJobs {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

struct CompletedJob {
    binding: ComputeJobBinding,
    phase: ComputeJobPhase,
    log_tail: Vec<u8>,
}

/// The content a worker staged to run one job, by where it came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputTransfer {
    /// Distinct input digests fetched from the requester.
    pub inputs: usize,
    /// Their bytes: what crossed the network.
    pub bytes: u64,
    /// Distinct input digests this worker already held, used once the
    /// requester proved it holds the same bytes: nothing was sent.
    pub held: usize,
    /// Their bytes.
    pub held_bytes: u64,
    /// Time spent staging them all, proofs included.
    pub elapsed: Duration,
}

/// Where a worker stages a job's inputs: a private per-job directory under
/// `root`, from content it already holds (`held`) where the requester
/// proves it may, and from the requester otherwise.
#[derive(Clone)]
pub struct InputStaging {
    /// Parent of the per-job directories.
    pub root: PathBuf,
    /// Content this worker already holds, if any.
    pub held: Option<Arc<dyn HeldContent>>,
}

impl From<PathBuf> for InputStaging {
    /// Staging that fetches every input from the requester.
    fn from(root: PathBuf) -> Self {
        Self { root, held: None }
    }
}

/// A job's inputs could not be hydrated.
#[derive(Debug)]
struct InputFailure {
    code: &'static str,
    detail: String,
}

impl InputFailure {
    fn fetch(digest: [u8; 32], error: &ContentFetchError) -> Self {
        let code = match error {
            ContentFetchError::TooLarge => "input_too_large",
            ContentFetchError::Integrity => "input_integrity",
            ContentFetchError::Refused(_)
            | ContentFetchError::Stream(_)
            | ContentFetchError::Store(_) => "input_unavailable",
        };
        Self {
            code,
            detail: format!("input {}: {error}", hex::encode(digest)),
        }
    }
}

/// Stage every input `spec` names, once each, into the private per-job
/// `store`, over the worker's authenticated compute `connection`.
///
/// An input this worker already holds (`held`) is staged from its own copy
/// when the job's requester proves, through the relay, that it holds the
/// same bytes ([`prove_job_input`]): the copy must hash to the digest *and*
/// match the proof under a fresh challenge, so a worker never runs a job on
/// content its invoker could not produce, and nothing is sent. Every other
/// input is asked for through the relay, which serves it from the job's
/// requester ([`crate::compute_content`]). Either way the worker stages
/// exactly what the job lists. The total is bounded by
/// [`MAX_COMPUTE_INPUT_BYTES`] and the job's memory limit, and the whole
/// staging by the job's lifetime.
pub async fn hydrate_job_inputs(
    connection: &iroh::endpoint::Connection,
    spec: &ComputeJobSpec,
    store: &dyn ComputeContentStore,
    held: Option<&Arc<dyn HeldContent>>,
) -> Result<InputTransfer, (&'static str, String)> {
    let started = std::time::Instant::now();
    let budget = MAX_COMPUTE_INPUT_BYTES.min(spec.limits.memory_bytes);
    let lifetime = Duration::from_millis(spec.expires_at_ms.saturating_sub(unix_now_ms()));
    let unique = spec.inputs.iter().copied().collect::<BTreeSet<_>>();
    let stage_all = async {
        let mut transfer = InputTransfer::default();
        let mut staged = BTreeSet::new();
        if let Some(held) = held {
            for (digest, bytes) in proven_held(connection, spec, &unique, held, budget).await {
                let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                if transfer.held_bytes.saturating_add(len) > budget {
                    // Over the bound together; the fetch below refuses it.
                    continue;
                }
                store.put(digest, &bytes).map_err(|detail| InputFailure {
                    code: "staging",
                    detail,
                })?;
                staged.insert(digest);
                transfer.held = transfer.held.saturating_add(1);
                transfer.held_bytes = transfer.held_bytes.saturating_add(len);
            }
        }
        for digest in unique.difference(&staged).copied() {
            let remaining = budget
                .saturating_sub(transfer.held_bytes)
                .saturating_sub(transfer.bytes);
            let bytes = fetch_job_content(
                connection,
                spec.workspace_id,
                spec.job_id,
                digest,
                remaining,
            )
            .await
            .map_err(|error| InputFailure::fetch(digest, &error))?;
            store.put(digest, &bytes).map_err(|detail| InputFailure {
                code: "staging",
                detail,
            })?;
            transfer.inputs = transfer.inputs.saturating_add(1);
            transfer.bytes = transfer
                .bytes
                .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        }
        Ok::<_, InputFailure>(transfer)
    };
    match tokio::time::timeout(lifetime, stage_all).await {
        Ok(Ok(mut transfer)) => {
            transfer.elapsed = started.elapsed();
            Ok(transfer)
        }
        Ok(Err(failure)) => Err((failure.code, failure.detail)),
        Err(_) => Err((
            "input_timeout",
            "inputs did not arrive within the job's lifetime".into(),
        )),
    }
}

/// The inputs of `spec` among `digests` that `held` has a copy of and the
/// job's requester proved it holds too, each at most `max_bytes`. Every
/// input is tried at once; one that is not held, not proven, or does not
/// match is left out and fetched instead.
async fn proven_held(
    connection: &iroh::endpoint::Connection,
    spec: &ComputeJobSpec,
    digests: &BTreeSet<[u8; 32]>,
    held: &Arc<dyn HeldContent>,
    max_bytes: u64,
) -> Vec<([u8; 32], Vec<u8>)> {
    let mut proving = tokio::task::JoinSet::new();
    for &digest in digests {
        let connection = connection.clone();
        let held = Arc::clone(held);
        let job = (spec.workspace_id, spec.job_id);
        proving.spawn(async move {
            let proven = proven_copy(&connection, job, digest, held, max_bytes).await;
            proven.map(|bytes| (digest, bytes))
        });
    }
    let mut proven = Vec::new();
    while let Some(joined) = proving.join_next().await {
        if let Ok(Some(copy)) = joined {
            proven.push(copy);
        }
    }
    proven
}

/// Read what `held` has as `digest` (by `locator` when given), off the
/// runtime; a read failure counts as not held.
async fn read_held(
    held: Arc<dyn HeldContent>,
    digest: [u8; 32],
    locator: Option<[u8; 32]>,
    max_bytes: u64,
) -> Option<Vec<u8>> {
    tokio::task::spawn_blocking(move || held.held(digest, locator, max_bytes))
        .await
        .unwrap_or_else(|_| Err("held content read panicked".into()))
        .inspect_err(|error| tracing::debug!(%error, "held compute input unreadable"))
        .ok()
        .flatten()
}

/// This worker's copy of input `digest` of `job`, when `held` has one and
/// the requester proves it holds the same bytes; `None` otherwise.
async fn proven_copy(
    connection: &iroh::endpoint::Connection,
    job: (u64, [u8; 16]),
    digest: [u8; 32],
    held: Arc<dyn HeldContent>,
    max_bytes: u64,
) -> Option<Vec<u8>> {
    // Look by digest first: a store that cannot locate by hint has nothing
    // to prove when it has nothing under the digest, so a miss costs no
    // round trip.
    let mut copy = read_held(Arc::clone(&held), digest, None, max_bytes).await;
    if copy.is_none() && !held.locates() {
        return None;
    }
    let mut nonce = [0_u8; 32];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut nonce);
    let (proof, locator) = match prove_job_input(connection, job, digest, nonce).await {
        Ok(answer) => answer,
        Err(error) => {
            tracing::debug!(%error, input = %hex::encode(digest), "input not proven; fetching it");
            return None;
        }
    };
    if copy.is_none() {
        copy = read_held(held, digest, Some(locator?), max_bytes).await;
    }
    // blake3's `Hash` compares in constant time.
    copy.filter(|bytes| {
        blake3::hash(bytes).as_bytes() == &digest
            && blake3::Hash::from_bytes(content_proof(&nonce, bytes))
                == blake3::Hash::from_bytes(proof)
    })
}

/// What running one assignment produced: the terminal phase and log the
/// worker reports, and what it fetched to get there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputeAssignment {
    pub phase: ComputeJobPhase,
    /// The bounded diagnostic tail to report.
    pub log_tail: Vec<u8>,
    /// The inputs hydrated for the job.
    pub transfer: InputTransfer,
}

/// Run one routed job end to end on `executor`: hydrate its inputs into a
/// private directory under the `staging` root ([`hydrate_job_inputs`]), execute
/// it in the package sandbox, and return what to report. Outputs stay in
/// the executor's store, retrievable by the job's requester through
/// [`ComputeOutputSource`]; the hydrated inputs are removed when the job
/// ends.
pub async fn run_compute_assignment<C, P, S>(
    executor: Arc<ComputeWorker<C, P, S>>,
    connection: iroh::endpoint::Connection,
    spec: ComputeJobSpec,
    staging: impl Into<InputStaging>,
    cancellation: CaptureCancellation,
) -> ComputeAssignment
where
    C: ComputeContentStore + 'static,
    P: ComputePackageCatalogue + 'static,
    S: ComputeSandbox + 'static,
{
    let failed = |code: &str, detail: String, transfer: InputTransfer| ComputeAssignment {
        phase: ComputeJobPhase::Failed { code: code.into() },
        log_tail: bounded_log(detail.into_bytes()),
        transfer,
    };
    let InputStaging { root, held } = staging.into();
    let job_inputs = root.join(hex::encode(spec.job_id));
    let _remove_inputs = RemoveOnDrop(job_inputs.clone());
    let (inputs, transfer): (Box<dyn ComputeContentStore>, InputTransfer) = if spec
        .inputs
        .is_empty()
    {
        (Box::new(NoInputs), InputTransfer::default())
    } else {
        let store = match FileComputeContentStore::open(job_inputs) {
            Ok(store) => store,
            Err(error) => return failed("staging", error.to_string(), InputTransfer::default()),
        };
        match hydrate_job_inputs(&connection, &spec, &store, held.as_ref()).await {
            Ok(transfer) => (Box::new(store), transfer),
            Err((code, detail)) => return failed(code, detail, InputTransfer::default()),
        }
    };
    let executed = tokio::task::spawn_blocking(move || {
        executor.execute_with_inputs(
            inputs.as_ref(),
            (spec.requester, spec.workspace_id),
            &spec,
            unix_now_ms(),
            cancellation,
        )
    })
    .await;
    let (phase, log_tail) = match executed {
        Ok(result) => sanitize_result(result),
        Err(error) => (
            ComputeJobPhase::Failed {
                code: "execution".into(),
            },
            bounded_log(error.to_string().into_bytes()),
        ),
    };
    ComputeAssignment {
        phase,
        log_tail,
        transfer,
    }
}

/// The inputs of a job that names none.
struct NoInputs;

impl ComputeContentStore for NoInputs {
    fn get(&self, _digest: [u8; 32]) -> Result<Option<Vec<u8>>, String> {
        Ok(None)
    }

    fn put(&self, _digest: [u8; 32], _bytes: &[u8]) -> Result<(), String> {
        Err("a job without inputs stages none".into())
    }
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(path = %self.0.display(), %error, "failed to remove hydrated compute inputs");
            }
        }
    }
}

/// Forget outputs whose retention has passed: the index of every job
/// completed longer than `retention` before `now`, then every retained
/// output no remaining index names. A just-written output is kept even
/// before its index lands, because only content older than `retention` is
/// removed.
///
/// # Errors
///
/// Returns an error when a directory cannot be read.
pub fn prune_retained_outputs(
    execution_root: &Path,
    content_root: &Path,
    now: std::time::SystemTime,
    retention: Duration,
) -> std::io::Result<()> {
    let expired = |entry: &fs::DirEntry| {
        entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| {
                now.duration_since(modified)
                    .is_ok_and(|age| age > retention)
            })
    };
    let mut referenced = std::collections::HashSet::new();
    match fs::read_dir(execution_root) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                if !name.to_string_lossy().ends_with(OUTPUT_INDEX_SUFFIX) {
                    continue;
                }
                if expired(&entry) {
                    fs::remove_file(entry.path())?;
                    continue;
                }
                let index = fs::read_to_string(entry.path())?;
                referenced.extend(index.lines().map(str::to_owned));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match fs::read_dir(content_root) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || referenced.contains(&name) || !expired(&entry) {
                    continue;
                }
                fs::remove_file(entry.path())?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

/// Remove the per-job scratch under a worker's `state_root` older than
/// `stale_after`: the inputs hydrated for a job (`inputs/<job>`) and its
/// verified package snapshot (`execution/jobs/<job>`). A job removes both
/// as it ends; a worker killed mid-job, with the relay it runs in, never
/// does. Returns how many were removed.
///
/// # Errors
///
/// Returns an error when a scratch directory cannot be read or an entry
/// cannot be removed.
pub fn prune_stale_job_scratch(
    state_root: &Path,
    now: std::time::SystemTime,
    stale_after: Duration,
) -> std::io::Result<usize> {
    let mut removed = 0_usize;
    for scratch in [
        state_root.join("inputs"),
        state_root.join("execution").join("jobs"),
    ] {
        let entries = match fs::read_dir(&scratch) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            // Not followed: a link is removed as itself.
            let metadata = entry.metadata()?;
            let stale = metadata.modified().is_ok_and(|modified| {
                now.duration_since(modified)
                    .is_ok_and(|age| age > stale_after)
            });
            if !stale {
                continue;
            }
            let removal = if metadata.is_dir() {
                fs::remove_dir_all(entry.path())
            } else {
                fs::remove_file(entry.path())
            };
            match removal {
                Ok(()) => removed = removed.saturating_add(1),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(removed)
}

/// The executor a production worker runs jobs on.
type ProductionExecutor =
    ComputeWorker<FileComputeContentStore, InstalledComputeCatalogue, LauncherComputeSandbox>;

async fn handle_delivery(
    client: &mut ComputeClient,
    config: &ProductionComputeWorkerConfig,
    health: &WorkerHealth,
    (executor, held): (&Arc<ProductionExecutor>, &WorkerHeldContent),
    completed: &tokio::sync::mpsc::Sender<CompletedJob>,
    active: &mut ActiveJobs,
    delivery: ComputeDelivery,
) -> Result<(), ComputeWorkerError> {
    match &delivery.envelope {
        ComputeEnvelope::Request { spec } => {
            let binding = ComputeJobBinding {
                workspace_id: spec.workspace_id,
                job_id: spec.job_id,
                requester: spec.requester,
                worker: spec.worker,
            };
            if let Some(existing) = active.get(&spec.job_id) {
                if existing.binding != binding {
                    return Err(ComputeWorkerError::Health(
                        "replayed job id has a different route binding".into(),
                    ));
                }
                client.acknowledge(&delivery).await?;
                return Ok(());
            }
            let assignment_capacity =
                usize::try_from(DEFAULT_ASSIGNMENT_CAPACITY).map_err(|_| {
                    ComputeWorkerError::Health("assignment capacity is not representable".into())
                })?;
            if active.len() >= assignment_capacity {
                return Err(ComputeWorkerError::Health(
                    "relay routed beyond advertised worker capacity".into(),
                ));
            }
            let cancellation = CaptureCancellation::default();
            active.insert(
                spec.job_id,
                ActiveJob {
                    binding: binding.clone(),
                    cancellation: cancellation.clone(),
                },
            );
            client
                .send(&ComputeEnvelope::Status {
                    update: ComputeStatusUpdate {
                        binding: binding.clone(),
                        sequence: 1,
                        phase: ComputeJobPhase::Running,
                        log_tail: Vec::new(),
                    },
                })
                .await?;
            client.acknowledge(&delivery).await?;
            advertise_all(client, config, health, active.len()).await?;
            let executor = Arc::clone(executor);
            let spec = spec.clone();
            let completed = completed.clone();
            let connection = client.connection();
            let staging = InputStaging {
                root: config.state_root.join("inputs"),
                held: Some(held.for_workspace(spec.workspace_id)),
            };
            tokio::spawn(async move {
                let job = hex::encode(spec.job_id);
                let assignment =
                    run_compute_assignment(executor, connection, spec, staging, cancellation).await;
                tracing::debug!(
                    %job,
                    inputs = assignment.transfer.inputs,
                    input_bytes = assignment.transfer.bytes,
                    held_inputs = assignment.transfer.held,
                    held_bytes = assignment.transfer.held_bytes,
                    hydration_ms = assignment.transfer.elapsed.as_millis(),
                    "compute job finished"
                );
                let _ = completed
                    .send(CompletedJob {
                        binding,
                        phase: assignment.phase,
                        log_tail: assignment.log_tail,
                    })
                    .await;
            });
        }
        ComputeEnvelope::Cancel { binding } => {
            cancel_active(active, binding)?;
            client.acknowledge(&delivery).await?;
        }
        ComputeEnvelope::Ping => {
            client.send(&ComputeEnvelope::Pong).await?;
        }
        ComputeEnvelope::Ready { .. } | ComputeEnvelope::Pong => {}
        _ => {
            return Err(ComputeWorkerError::Health(
                "worker received an unexpected compute envelope".into(),
            ));
        }
    }
    Ok(())
}

async fn send_completion(
    client: &mut ComputeClient,
    completed: CompletedJob,
) -> Result<(), ComputeWorkerError> {
    client
        .send(&ComputeEnvelope::Result {
            result: ComputeResultUpdate {
                binding: completed.binding,
                sequence: 2,
                phase: completed.phase,
                log_tail: completed.log_tail,
            },
        })
        .await?;
    Ok(())
}

/// The phase and bounded log a finished execution reports. A completed
/// job's outputs are reported as they are: [`ComputeWorker`] retained each
/// for the job's requester before completing.
fn sanitize_result(
    result: Result<ComputeJobResult, ComputeExecutionError>,
) -> (ComputeJobPhase, Vec<u8>) {
    match result {
        Ok(result) => {
            let mut log = result.stdout;
            log.extend_from_slice(&result.stderr);
            (result.lifecycle.phase, bounded_log(log))
        }
        Err(error) => (
            ComputeJobPhase::Failed {
                code: error.code().into(),
            },
            bounded_log(error.to_string().into_bytes()),
        ),
    }
}

fn cancel_active(
    active: &ActiveJobs,
    binding: &ComputeJobBinding,
) -> Result<bool, ComputeWorkerError> {
    let Some(job) = active.get(&binding.job_id) else {
        return Ok(false);
    };
    if job.binding != *binding {
        return Err(ComputeWorkerError::Health(
            "cancel binding differs from active job".into(),
        ));
    }
    job.cancellation.cancel();
    Ok(true)
}

fn health_unchanged(expected: &WorkerHealth, refreshed: &WorkerHealth) -> bool {
    expected.apps == refreshed.apps && expected.power == refreshed.power
}

async fn advertise_all(
    client: &mut ComputeClient,
    config: &ProductionComputeWorkerConfig,
    health: &WorkerHealth,
    active_assignments: usize,
) -> Result<(), ComputeWorkerError> {
    let issued_at_ms = unix_now_ms();
    let active_assignments = u32::try_from(active_assignments).unwrap_or(u32::MAX);
    let (environment_capacity, active_environments) = config.environments.capacity();
    for workspace_id in &config.workspaces {
        client
            .send(&ComputeEnvelope::Advertise {
                workspace_id: *workspace_id,
                advertisement: ComputeWorkerAdvertisement {
                    version: COMPUTE_TRANSPORT_SCHEMA_VERSION,
                    worker: config.device_cert.device_public_key,
                    workspace_id: *workspace_id,
                    apps: health.apps.clone(),
                    accelerators: config.environments.accelerators(),
                    assignment_capacity: DEFAULT_ASSIGNMENT_CAPACITY,
                    active_assignments,
                    environment_capacity,
                    active_environments,
                    power: health.power,
                    energy_saver: false,
                    issued_at_ms,
                    expires_at_ms: issued_at_ms.saturating_add(ADVERTISEMENT_LIFETIME_MS),
                },
            })
            .await?;
    }
    Ok(())
}

fn server_advertisement(
    config: &ProductionComputeWorkerConfig,
    health: &WorkerHealth,
    active_assignments: u32,
) -> ServerAdvertisement {
    ServerAdvertisement {
        device: config.device_cert.device_public_key,
        role: DeviceRole::AlwaysOnServer,
        services: ServiceCapabilities {
            compute: true,
            ..ServiceCapabilities::default()
        },
        workspaces: config.workspaces.clone(),
        apps: health.apps.clone(),
        accelerators: config.environments.accelerators(),
        priority: 0,
        latency_ms: 0,
        power: health.power,
        energy_saver: false,
        active_assignments,
        assignment_capacity: DEFAULT_ASSIGNMENT_CAPACITY,
        online: true,
    }
}

fn inspect_health(hosts_environments: bool) -> Result<WorkerHealth, ComputeWorkerError> {
    if !bubblewrap_available() {
        return Err(ComputeWorkerError::Health(
            "bubblewrap launcher is unavailable".into(),
        ));
    }
    if Path::new("/run/tilo/energy-saver").exists() {
        return Err(ComputeWorkerError::Health(
            "system energy saver is active".into(),
        ));
    }
    let power = read_power_availability();
    if matches!(power, PowerAvailability::Battery(percent) if percent <= 15) {
        return Err(ComputeWorkerError::Health(
            "battery is critically constrained".into(),
        ));
    }
    let mut apps = BTreeSet::new();
    for package in package_store::list_installed()
        .map_err(|error| ComputeWorkerError::Health(error.to_string()))?
    {
        let Ok(report) = package_store::integrity::verify(&package, &package.payload_root) else {
            continue;
        };
        let Ok(manifest) = read_manifest(&package.manifest_path) else {
            continue;
        };
        if report.is_intact()
            && report.unexpected.is_empty()
            && manifest.package_id == package.id
            && !manifest.cli_commands.is_empty()
            && manifest.entrypoint.cli_bin().is_some()
        {
            apps.insert(package.id);
        }
    }
    if apps.is_empty() && !hosts_environments {
        return Err(ComputeWorkerError::Health(
            "no intact signed package exposes a CLI compute entrypoint".into(),
        ));
    }
    Ok(WorkerHealth { apps, power })
}

fn bubblewrap_available() -> bool {
    ["/usr/bin/bwrap", "/bin/bwrap"]
        .iter()
        .any(|path| Path::new(path).is_file())
}

fn read_power_availability() -> PowerAvailability {
    let root = Path::new("/sys/class/power_supply");
    let Ok(entries) = fs::read_dir(root) else {
        return PowerAvailability::External;
    };
    let mut battery = None;
    let mut external = false;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let kind = fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim() == "Battery" {
            let capacity = fs::read_to_string(path.join("capacity"))
                .ok()
                .and_then(|value| value.trim().parse::<u8>().ok())
                .map(|value| value.min(100));
            battery = capacity;
        } else if fs::read_to_string(path.join("online")).is_ok_and(|value| value.trim() == "1") {
            external = true;
        }
    }
    match (battery, external) {
        (Some(percent), true) => PowerAvailability::Charging(percent),
        (Some(percent), false) => PowerAvailability::Battery(percent),
        (None, _) => PowerAvailability::External,
    }
}

fn bounded_log(mut bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() > MAX_COMPUTE_LOG_BYTES {
        bytes.drain(..bytes.len().saturating_sub(MAX_COMPUTE_LOG_BYTES));
    }
    bytes
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct ReadinessGuard(Arc<AtomicBool>);

impl Drop for ReadinessGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker killed mid-job leaves that job's staged inputs and package
    /// snapshot behind; once older than any job can live they are removed,
    /// and nothing else in the worker's state is.
    #[test]
    fn a_killed_jobs_scratch_is_removed_once_no_job_can_use_it() {
        let root = tempfile::tempdir().unwrap();
        let state_root = root.path().join("compute-worker");
        assert_eq!(
            prune_stale_job_scratch(&state_root, std::time::SystemTime::now(), STALE_JOB_SCRATCH)
                .unwrap(),
            0,
            "a worker with no scratch yet has nothing to remove"
        );
        let inputs = state_root.join("inputs").join("0a".repeat(16));
        let snapshot = state_root
            .join("execution")
            .join("jobs")
            .join("0b".repeat(16));
        fs::create_dir_all(&inputs).unwrap();
        fs::write(inputs.join("0c".repeat(32)), b"input").unwrap();
        fs::create_dir_all(snapshot.join("payload").join("bin")).unwrap();
        fs::write(snapshot.join("manifest.toml"), b"manifest").unwrap();
        let claim = state_root
            .join("execution")
            .join(format!("{}.claim", "0b".repeat(16)));
        fs::write(&claim, b"claim").unwrap();
        let retained = state_root.join("content").join("0d".repeat(32));
        fs::create_dir_all(retained.parent().unwrap()).unwrap();
        fs::write(&retained, b"output").unwrap();

        let now = std::time::SystemTime::now();
        assert_eq!(
            prune_stale_job_scratch(&state_root, now, STALE_JOB_SCRATCH).unwrap(),
            0
        );
        assert!(inputs.exists() && snapshot.exists());

        let later = now
            .checked_add(STALE_JOB_SCRATCH.saturating_add(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(
            prune_stale_job_scratch(&state_root, later, STALE_JOB_SCRATCH).unwrap(),
            2
        );
        assert!(!inputs.exists() && !snapshot.exists());
        assert!(
            claim.exists() && retained.exists(),
            "replay claims and retained outputs are not scratch"
        );
        assert_eq!(
            prune_stale_job_scratch(&state_root, later, STALE_JOB_SCRATCH).unwrap(),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replica_s_small_files_are_held_only_for_that_workspace_s_jobs() {
        // Two workspaces' replicas, each serving its own store as
        // `fs replica` does, each holding one small file.
        let root = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        let mut servers = Vec::new();
        for (workspace, text) in [(7_u64, "a photo in seven"), (8, "a photo in eight")] {
            let dir = root.path().join(format!("replica-{workspace}"));
            let store = Arc::new(::fs::store::ContentStore::open(&dir).await.unwrap());
            let bytes = text.as_bytes().to_vec();
            let manifest = *store
                .write_content(&bytes)
                .await
                .unwrap()
                .unwrap()
                .as_bytes();
            let listener =
                tilo_files::native::private::bind_socket(&::fs::replica_content::socket_path(&dir))
                    .unwrap();
            listener.set_nonblocking(true).unwrap();
            servers.push(tokio::spawn(::fs::replica_content::serve(
                tokio::net::UnixListener::from_std(listener).unwrap(),
                store,
            )));
            files.push((workspace, dir, bytes, manifest));
        }
        let outputs = FileComputeContentStore::open(root.path().join("content")).unwrap();
        let replicas = files
            .iter()
            .map(|(workspace, dir, _, _)| (*workspace, vec![dir.clone()]))
            .collect::<BTreeMap<_, _>>();
        let held = WorkerHeldContent::new(Arc::new(outputs), &replicas);

        for (workspace, _, bytes, manifest) in &files {
            for (other, ..) in &files {
                let digest = *blake3::hash(bytes).as_bytes();
                let source = held.for_workspace(*other);
                let manifest = *manifest;
                let read =
                    tokio::task::spawn_blocking(move || source.held(digest, Some(manifest), 1024))
                        .await
                        .unwrap();
                if other == workspace {
                    assert_eq!(read, Ok(Some(bytes.clone())), "its own workspace's job");
                } else {
                    assert_eq!(read, Ok(None), "another workspace's file is never served");
                }
            }
        }
        for server in servers {
            server.abort();
        }
    }

    #[test]
    fn a_workspace_replica_is_held_only_for_that_workspace_s_jobs() {
        let root = tempfile::tempdir().unwrap();
        let replica = root.path().join("replica");
        let file: Vec<u8> = (0..2 * 1024 * 1024_u32)
            .map(|index| u8::try_from(index % 239).unwrap())
            .collect();
        let file_digest = *blake3::hash(&file).as_bytes();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let store = runtime
            .block_on(::fs::store::ContentStore::open(&replica))
            .unwrap();
        let manifest = *runtime
            .block_on(store.write_content(&file))
            .unwrap()
            .unwrap()
            .as_bytes();
        let outputs = FileComputeContentStore::open(root.path().join("content")).unwrap();
        let output = b"a retained output".to_vec();
        let output_digest = *blake3::hash(&output).as_bytes();
        outputs.put(output_digest, &output).unwrap();
        let held = WorkerHeldContent::new(Arc::new(outputs), &BTreeMap::from([(7, vec![replica])]));

        let own = held.for_workspace(7);
        assert!(own.locates());
        assert_eq!(
            own.held(file_digest, Some(manifest), u64::MAX),
            Ok(Some(file))
        );
        assert_eq!(own.held(file_digest, None, u64::MAX), Ok(None));
        assert_eq!(
            own.held(output_digest, None, u64::MAX),
            Ok(Some(output.clone()))
        );
        // Another workspace's job never reaches this replica, only the
        // outputs every job may chain on.
        let other = held.for_workspace(8);
        assert!(!other.locates());
        assert_eq!(other.held(file_digest, Some(manifest), u64::MAX), Ok(None));
        assert_eq!(other.held(output_digest, None, u64::MAX), Ok(Some(output)));
        drop(store);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn compute_worker_state_is_bounded_withdrawn_and_revoked_with_its_owner() {
        {
            let root = tempfile::tempdir().unwrap();
            let store = FileComputeContentStore::open(root.path().join("content")).unwrap();
            let bytes = b"bounded content";
            let digest = *blake3::hash(bytes).as_bytes();
            store.put(digest, bytes).unwrap();
            assert_eq!(store.get(digest).unwrap(), Some(bytes.to_vec()));
            fs::write(store.path_for(digest), b"substitution").unwrap();
            assert!(store.get(digest).is_err());
        }
        {
            let bytes = (0..MAX_COMPUTE_LOG_BYTES + 9)
                .map(|index| u8::try_from(index % 251).unwrap())
                .collect::<Vec<_>>();
            let expected = bytes[9..].to_vec();
            assert_eq!(bounded_log(bytes), expected);
        }
        {
            let readiness = Arc::new(AtomicBool::new(true));
            drop(ReadinessGuard(Arc::clone(&readiness)));
            assert!(!readiness.load(Ordering::Acquire));
        }
        {
            let healthy = WorkerHealth {
                apps: BTreeSet::from(["org.tilo.Compute".into()]),
                power: PowerAvailability::External,
            };
            let constrained = WorkerHealth {
                apps: healthy.apps.clone(),
                power: PowerAvailability::Battery(10),
            };
            assert!(health_unchanged(&healthy, &healthy));
            assert!(!health_unchanged(&healthy, &constrained));
        }
        {
            let binding = ComputeJobBinding {
                workspace_id: 7,
                job_id: [8; 16],
                requester: [9; 32],
                worker: [10; 32],
            };
            let cancellation = CaptureCancellation::default();
            let active = ActiveJobs {
                by_job_id: HashMap::from([(
                    binding.job_id,
                    ActiveJob {
                        binding: binding.clone(),
                        cancellation: cancellation.clone(),
                    },
                )]),
            };
            let mut forged = binding.clone();
            forged.workspace_id = 11;
            assert!(cancel_active(&active, &forged).is_err());
            assert!(cancel_active(&active, &binding).unwrap());
            assert!(cancellation.is_cancelled());
        }
        {
            let first = ComputeJobBinding {
                workspace_id: 7,
                job_id: [1; 16],
                requester: [2; 32],
                worker: [3; 32],
            };
            let second = ComputeJobBinding {
                workspace_id: 8,
                job_id: [4; 16],
                requester: [5; 32],
                worker: [6; 32],
            };
            let first_cancellation = CaptureCancellation::default();
            let second_cancellation = CaptureCancellation::default();
            let mut active = ActiveJobs::default();
            active.insert(
                first.job_id,
                ActiveJob {
                    binding: first,
                    cancellation: first_cancellation.clone(),
                },
            );
            active.insert(
                second.job_id,
                ActiveJob {
                    binding: second,
                    cancellation: second_cancellation.clone(),
                },
            );

            assert_eq!(active.cancel_all(), 2);
            assert!(active.is_empty());
            assert!(first_cancellation.is_cancelled());
            assert!(second_cancellation.is_cancelled());
        }
        {
            let cancellation = CaptureCancellation::default();
            {
                let binding = ComputeJobBinding {
                    workspace_id: 9,
                    job_id: [10; 16],
                    requester: [11; 32],
                    worker: [12; 32],
                };
                let mut active = ActiveJobs::default();
                active.insert(
                    binding.job_id,
                    ActiveJob {
                        binding,
                        cancellation: cancellation.clone(),
                    },
                );
            }
            assert!(cancellation.is_cancelled());
        }
        {
            let result = ComputeJobResult {
                lifecycle: crate::compute::ComputeJobLifecycle {
                    job_id: [4; 16],
                    sequence: 2,
                    phase: ComputeJobPhase::Completed {
                        outputs: vec![[3; 32]],
                    },
                },
                stdout: b"out".to_vec(),
                stderr: b"err".to_vec(),
            };
            let (phase, log) = sanitize_result(Ok(result));
            assert_eq!(
                phase,
                ComputeJobPhase::Completed {
                    outputs: vec![[3; 32]]
                }
            );
            assert_eq!(log, b"outerr");
            let (phase, log) = sanitize_result(Err(ComputeExecutionError::MissingInput));
            assert_eq!(
                phase,
                ComputeJobPhase::Failed {
                    code: "input_unavailable".into()
                }
            );
            assert!(String::from_utf8(log).unwrap().contains("unavailable"));
        }
    }

    #[test]
    fn retention_forgets_expired_outputs_and_keeps_live_ones() {
        let root = tempfile::tempdir().unwrap();
        let execution = root.path().join("execution");
        let content = root.path().join("content");
        fs::create_dir_all(&execution).unwrap();
        fs::create_dir_all(&content).unwrap();
        let now = std::time::SystemTime::now();
        let long_ago = now
            .checked_sub(Duration::from_secs(2 * 24 * 60 * 60))
            .unwrap();
        let write = |path: PathBuf, bytes: &[u8], modified| {
            fs::write(&path, bytes).unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        };
        let live = hex::encode([1; 32]);
        let expired = hex::encode([2; 32]);
        let fresh = hex::encode([3; 32]);
        write(
            execution.join(format!("aa{OUTPUT_INDEX_SUFFIX}")),
            format!("{live}\n").as_bytes(),
            now,
        );
        write(
            execution.join(format!("bb{OUTPUT_INDEX_SUFFIX}")),
            format!("{expired}\n").as_bytes(),
            long_ago,
        );
        write(execution.join("bb.claim"), b"claim", long_ago);
        write(content.join(&live), b"live", long_ago);
        write(content.join(&expired), b"expired", long_ago);
        write(content.join(&fresh), b"fresh", now);

        prune_retained_outputs(&execution, &content, now, OUTPUT_RETENTION).unwrap();

        assert!(execution.join(format!("aa{OUTPUT_INDEX_SUFFIX}")).exists());
        assert!(!execution.join(format!("bb{OUTPUT_INDEX_SUFFIX}")).exists());
        assert!(
            execution.join("bb.claim").exists(),
            "claims outlive retention: they refuse a replayed job id"
        );
        assert!(
            content.join(&live).exists(),
            "a live index keeps its output"
        );
        assert!(!content.join(&expired).exists());
        assert!(
            content.join(&fresh).exists(),
            "an output written before its index lands is kept"
        );
    }
}
