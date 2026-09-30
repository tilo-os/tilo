//! Production owner for the signed-package compute executor.
//!
//! The owner advertises only after verifying the installed catalogue,
//! launcher, private content store, and local power policy. Its advertisement
//! lives on the dedicated authenticated session, so task failure, health
//! regression, shutdown, or connection loss withdraws eligibility without a
//! stale durable capability.

use std::collections::{BTreeSet, HashMap};
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
    ComputeContentStore, ComputeJobPhase, ComputeJobResult, ComputeWorker,
    InstalledComputeCatalogue, LauncherComputeSandbox, MAX_COMPUTE_INPUT_BYTES,
};
use crate::compute_session::{ComputeClient, ComputeDelivery, ComputeSessionError};
use crate::compute_transport::{
    COMPUTE_TRANSPORT_SCHEMA_VERSION, ComputeEnvelope, ComputeJobBinding, ComputeResultUpdate,
    ComputeStatusUpdate, ComputeWorkerAdvertisement, MAX_COMPUTE_LOG_BYTES,
};
use crate::mesh::{
    ComputeAccelerator, DeviceRole, PowerAvailability, ServerAdvertisement, ServiceCapabilities,
};

const HEALTH_INTERVAL: Duration = Duration::from_secs(10);
const ADVERTISEMENT_LIFETIME_MS: u64 = 30_000;
const DEFAULT_ASSIGNMENT_CAPACITY: u32 = 1;

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
}

struct WorkerHealth {
    apps: BTreeSet<String>,
    power: PowerAvailability,
}

/// Run the production worker until shutdown, health regression, or transport
/// failure. Dropping/aborting this future closes the session and withdraws
/// every advertisement.
///
/// # Errors
///
/// Returns an error if health validation, authentication, execution routing,
/// or the dedicated session fails.
pub async fn run_production_compute_worker(
    config: ProductionComputeWorkerConfig,
) -> Result<(), ComputeWorkerError> {
    let health = inspect_health()?;
    if config.workspaces.is_empty() {
        return Err(ComputeWorkerError::Health(
            "worker has no configured workspace".into(),
        ));
    }
    let data_home = launcher::appdir::host_data_home()
        .ok_or_else(|| ComputeWorkerError::Health("launcher has no trusted data home".into()))?;
    let content = FileComputeContentStore::open(config.state_root.join("content"))?;
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
    for workspace in &config.workspaces {
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

    loop {
        tokio::select! {
            delivery = client.receive() => {
                let delivery = delivery?;
                handle_delivery(
                    &mut client,
                    &config,
                    &health,
                    &executor,
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
                let refreshed = inspect_health()?;
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
    result: Result<ComputeJobResult, String>,
}

async fn handle_delivery(
    client: &mut ComputeClient,
    config: &ProductionComputeWorkerConfig,
    health: &WorkerHealth,
    executor: &Arc<
        ComputeWorker<FileComputeContentStore, InstalledComputeCatalogue, LauncherComputeSandbox>,
    >,
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
            if !spec.inputs.is_empty() {
                client.acknowledge(&delivery).await?;
                client
                    .send(&ComputeEnvelope::Result {
                        result: ComputeResultUpdate {
                            binding,
                            sequence: 1,
                            phase: ComputeJobPhase::Failed {
                                code: "content_plane_unavailable".into(),
                            },
                            log_tail: b"remote content hydration is not enabled on this worker"
                                .to_vec(),
                        },
                    })
                    .await?;
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
            tokio::task::spawn_blocking(move || {
                let result = executor
                    .execute(
                        spec.requester,
                        spec.workspace_id,
                        &spec,
                        unix_now_ms(),
                        cancellation,
                    )
                    .map_err(|error| error.to_string());
                let _ = completed.blocking_send(CompletedJob { binding, result });
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
    let (phase, log) = sanitize_result(completed.result);
    client
        .send(&ComputeEnvelope::Result {
            result: ComputeResultUpdate {
                binding: completed.binding,
                sequence: 2,
                phase,
                log_tail: log,
            },
        })
        .await?;
    Ok(())
}

fn sanitize_result(result: Result<ComputeJobResult, String>) -> (ComputeJobPhase, Vec<u8>) {
    match result {
        Ok(result) => {
            let mut log = result.stdout;
            log.extend_from_slice(&result.stderr);
            let phase = match result.lifecycle.phase {
                ComputeJobPhase::Completed { ref outputs } if !outputs.is_empty() => {
                    log.extend_from_slice(
                        b"\nremote output retrieval is not enabled on this worker",
                    );
                    ComputeJobPhase::Failed {
                        code: "content_plane_unavailable".into(),
                    }
                }
                phase => phase,
            };
            (phase, bounded_log(log))
        }
        Err(error) => (
            ComputeJobPhase::Failed {
                code: "execution".into(),
            },
            bounded_log(error.into_bytes()),
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
    for workspace_id in &config.workspaces {
        client
            .send(&ComputeEnvelope::Advertise {
                workspace_id: *workspace_id,
                advertisement: ComputeWorkerAdvertisement {
                    version: COMPUTE_TRANSPORT_SCHEMA_VERSION,
                    worker: config.device_cert.device_public_key,
                    workspace_id: *workspace_id,
                    apps: health.apps.clone(),
                    gpu: false,
                    assignment_capacity: DEFAULT_ASSIGNMENT_CAPACITY,
                    active_assignments,
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
        accelerators: BTreeSet::<ComputeAccelerator>::new(),
        priority: 0,
        latency_ms: 0,
        power: health.power,
        energy_saver: false,
        active_assignments,
        assignment_capacity: DEFAULT_ASSIGNMENT_CAPACITY,
        online: true,
    }
}

fn inspect_health() -> Result<WorkerHealth, ComputeWorkerError> {
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
    if apps.is_empty() {
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

struct ReadinessGuard(Arc<AtomicBool>);

impl Drop for ReadinessGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn compute_worker_state_is_bounded_withdrawn_and_revoked_with_its_owner() {
        // Phase `file_content_store_rejects_digest_substitution`.
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
        // Phase `log_tail_is_bounded_to_latest_diagnostics`.
        {
            let bytes = (0..MAX_COMPUTE_LOG_BYTES + 9)
                .map(|index| u8::try_from(index % 251).unwrap())
                .collect::<Vec<_>>();
            let expected = bytes[9..].to_vec();
            assert_eq!(bounded_log(bytes), expected);
        }
        // Phase `readiness_is_withdrawn_when_owner_stops`.
        {
            let readiness = Arc::new(AtomicBool::new(true));
            drop(ReadinessGuard(Arc::clone(&readiness)));
            assert!(!readiness.load(Ordering::Acquire));
        }
        // Phase `health_regression_requires_withdrawal`.
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
        // Phase `cancellation_requires_exact_active_route`.
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
        // Phase `shutdown_cancels_all_active_work_and_clears_exact_routes`.
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
        // Phase `dropping_active_registry_revokes_running_work`.
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
        // Phase `inaccessible_outputs_are_never_advertised_as_retrievable`.
        {
            let result = ComputeJobResult {
                lifecycle: crate::compute::ComputeJobLifecycle {
                    job_id: [4; 16],
                    sequence: 2,
                    phase: ComputeJobPhase::Completed {
                        outputs: vec![[3; 32]],
                    },
                },
                stdout: Vec::new(),
                stderr: Vec::new(),
            };
            let (phase, log) = sanitize_result(Ok(result));
            assert_eq!(
                phase,
                ComputeJobPhase::Failed {
                    code: "content_plane_unavailable".into()
                }
            );
            assert!(String::from_utf8(log).unwrap().contains("output retrieval"));
        }
    }
}
